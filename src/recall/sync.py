"""Mac→fleet sync — the security core of the proposed Isis/Mac split.

See `docs/isis-migration.md`. The Mac is a one-way WireGuard peer: it may dial the
fleet, nothing may dial back. So every exchange is **Mac-initiated** — the Mac POLLS the
fleet for jobs (it has the ML) and PUSHES results to the fleet's system of record. This
module is the transport + auth for that inversion.

It is **inert unless `RECALL_SYNC_TOKEN` is set**: importing it changes nothing, and the
routes are only registered when a token is configured, so a stock LAN-only deployment
is untouched. When enabled, the routes are meant to bind to the WireGuard interface only
— never the shared public ingress, which answers on the public IP regardless of DNS.

The whole Mac→fleet flow is here: job poll, audio-blob push, segment/turns push
(supersede-aware), and day-summary push. The auth check and bearer parsing are pure, so
they're unit-tested; routes and client are exercised against a FastAPI test transport.
"""

from __future__ import annotations

import hmac
import os
import shutil
from collections.abc import Callable
from datetime import datetime
from pathlib import Path

import httpx
from fastapi import FastAPI, File, Form, Header, HTTPException, UploadFile
from pydantic import BaseModel

from recall.schemas import OkOut
from recall.sources import AudioSource, SourceKind
from recall.store import RefineRequest, Store, TranscriptSegment
from recall.timeline import Segment

SYNC_TOKEN_ENV = "RECALL_SYNC_TOKEN"
_BEARER = "Bearer "


def _safe_component(component: str) -> str:
    """A single path component the fleet will trust as a directory/file name. Rejects
    anything that could escape the archive root (separators, `..`, a hidden dot-file) —
    the Mac is authenticated, but a compromised token must not become path traversal."""
    if (
        not component
        or "/" in component
        or "\\" in component
        or ".." in component
        or component.startswith(".")
    ):
        raise HTTPException(
            status_code=400, detail=f"unsafe path component: {component!r}"
        )
    return component


def sync_token() -> str | None:
    """The secret the Mac presents to the fleet; None when the split is off."""
    return os.environ.get(SYNC_TOKEN_ENV)


def bearer(header: str | None) -> str | None:
    """The token from an ``Authorization: Bearer <token>`` header, or None."""
    if header and header.startswith(_BEARER):
        return header[len(_BEARER) :]
    return None


def check_token(presented: str | None, expected: str | None) -> None:
    """Authorise a sync request. 503 when the server has no token configured (the split
    is off — never silently accept), 401 when the header is missing or wrong. Constant-
    time compare, so a wrong token leaks no timing signal."""
    if not expected:
        raise HTTPException(status_code=503, detail="sync not enabled")
    if not presented or not hmac.compare_digest(presented, expected):
        raise HTTPException(status_code=401, detail="bad sync token")


class JobOut(BaseModel):
    """A unit of work the fleet hands the Mac worker (the Mac has the ML + the mic)."""

    id: int
    type: str
    source: str
    start: str  # ISO-8601
    end: str


class AudioStoredOut(BaseModel):
    """Whether the fleet newly stored the pushed segment. False = it already had it."""

    stored: bool


class AudioPresentOut(BaseModel):
    """Whether the fleet already holds this file — lets the Mac skip re-sending the
    (immutable) blob's bytes on every sync pass."""

    present: bool


class TurnIn(BaseModel):
    """One transcript turn the Mac computed for a segment, for the fleet's store."""

    start: str  # ISO-8601
    end: str
    text: str
    asr_model: str
    language: str | None = None
    asr_confidence: float | None = None
    speaker_cluster: str | None = None
    provenance: str | None = None


class SegmentIn(BaseModel):
    """A processed audio segment the Mac pushes to the fleet: the segment metadata (its
    audio blob is pushed separately, by path) plus the turns transcribed from it."""

    source_id: str
    source_name: str
    kind: str  # a SourceKind value
    path: str
    start: str  # ISO-8601
    end: str
    sample_rate: int
    channels: int
    turns: list[TurnIn]


class SegmentStoredOut(BaseModel):
    """The fleet's audio-segment id, and how many turns it wrote (0 = already had)."""

    audio_segment_id: int
    turns_written: int


class SummaryIn(BaseModel):
    """A settled day-summary the Mac's LLM generated, for the fleet's Ask page."""

    day: str  # YYYY-MM-DD
    text: str
    model: str


def _incoming_turn_keys(turns: list[TurnIn]) -> list[tuple[str, str, str, str]]:
    """An order-independent identity for a pushed turn set — to skip a no-op re-push."""
    return sorted((t.start, t.end, t.text, t.asr_model) for t in turns)


def _machine_turn_keys(
    turns: list[TranscriptSegment],
) -> list[tuple[str, str, str, str]]:
    """The same identity for the fleet's current machine turns, to compare against."""
    return sorted(
        (t.start.isoformat(), t.end.isoformat(), t.text, t.asr_model) for t in turns
    )


def _job_of(req: RefineRequest) -> JobOut:
    return JobOut(
        id=req.id,
        type="refine",
        source=req.source,
        start=req.start.isoformat(),
        end=req.end.isoformat(),
    )


def _ingest_segment(store: Store, body: SegmentIn, data_root: Path) -> SegmentStoredOut:
    """Persist a pushed segment, reconciling across the split. The fleet is the system
    of record: a newer machine pass (worker → refine) SUPERSEDES the old machine turns,
    while human edits made on the fleet are authoritative and preserved — the same rule
    refine._replace_turns uses. An identical re-push is a no-op, so it never churns."""
    try:
        kind = SourceKind(body.kind)
    except ValueError as exc:
        raise HTTPException(status_code=400, detail=f"bad kind {body.kind!r}") from exc
    store.add_source(
        AudioSource(id=body.source_id, name=body.source_name, kind=kind, spec="")
    )
    seg_start = datetime.fromisoformat(body.start)
    seg_end = datetime.fromisoformat(body.end)
    # Re-home the path. The sender's `path` is absolute on the machine that recorded
    # it (`/Volumes/Backup/recall/usb/…` on the Mac), and storing it verbatim gave the
    # fleet a database describing a filesystem it cannot see: the transcripts read
    # perfectly and every play button 404s, silently, for ever. The blob itself lands
    # under the fleet's own root (see /sync/audio), so the row must point there too.
    # The fleet owns its archive layout; only the filename survives the trip — and it
    # is checked: an authenticated Mac is still hostile input if the token ever leaks.
    path = (
        data_root
        / _safe_component(body.source_id)
        / _safe_component(Path(body.path).name)
    )
    audio_id = store.add_audio_segment(
        Segment(
            source_id=body.source_id,
            sequence=0,
            start=seg_start,
            end=seg_end,
            path=str(path),
            sample_rate=body.sample_rate,
            channels=body.channels,
        )
    )
    current = _machine_turn_keys(store.visible_machine_turns_for_audio(audio_id))
    if current == _incoming_turn_keys(body.turns):
        return SegmentStoredOut(audio_segment_id=int(audio_id), turns_written=0)
    human = store.human_corrections_overlapping(int(audio_id), seg_start, seg_end)
    written = 0
    with store.transaction():
        for old in store.visible_machine_turns_for_audio(audio_id):
            store.hide(old.id, "superseded by sync push")
        for turn in body.turns:
            start = datetime.fromisoformat(turn.start)
            end = datetime.fromisoformat(turn.end)
            if any(c.start < end and c.end > start for c in human):
                continue  # human ground truth already covers this span
            store.add_transcript_segment(
                audio_segment_id=int(audio_id),
                start=start,
                end=end,
                text=turn.text,
                asr_model=turn.asr_model,
                language=turn.language,
                asr_confidence=turn.asr_confidence,
                speaker_cluster=turn.speaker_cluster,
                provenance=turn.provenance,
            )
            written += 1
    return SegmentStoredOut(audio_segment_id=int(audio_id), turns_written=written)


def register_sync_routes(
    app: FastAPI, store_factory: Callable[[], Store], data_root: Path
) -> bool:
    """Register the token-gated sync endpoints on `app`, but only when a token is
    configured — so a stock deployment is unchanged. Returns whether they were added.
    `data_root` is the archive root the Mac's audio blobs are streamed into."""
    expected = sync_token()
    if not expected:
        return False

    @app.post("/sync/audio")
    def sync_audio(
        source: str = Form(...),
        name: str = Form(...),
        file: UploadFile = File(...),
        authorization: str | None = Header(default=None),
    ) -> AudioStoredOut:
        check_token(bearer(authorization), expected)
        dest_dir = data_root / _safe_component(source)
        dest = dest_dir / _safe_component(name)
        # The archive is immutable (append-only, same path = same content), so an
        # existing file is never overwritten — the push is idempotent and safe to retry.
        if dest.exists():
            return AudioStoredOut(stored=False)
        dest_dir.mkdir(parents=True, exist_ok=True)
        with dest.open("wb") as fh:
            shutil.copyfileobj(file.file, fh)
        return AudioStoredOut(stored=True)

    @app.get("/sync/audio")
    def sync_audio_present(
        source: str, name: str, authorization: str | None = Header(default=None)
    ) -> AudioPresentOut:
        check_token(bearer(authorization), expected)
        dest = data_root / _safe_component(source) / _safe_component(name)
        return AudioPresentOut(present=dest.exists())

    @app.post("/sync/segments")
    def sync_segments(
        body: SegmentIn, authorization: str | None = Header(default=None)
    ) -> SegmentStoredOut:
        check_token(bearer(authorization), expected)
        store = store_factory()
        try:
            return _ingest_segment(store, body, data_root)
        finally:
            store.close()

    @app.post("/sync/summaries")
    def sync_summaries(
        body: SummaryIn, authorization: str | None = Header(default=None)
    ) -> OkOut:
        check_token(bearer(authorization), expected)
        store = store_factory()
        try:
            # Keyed on the day (PK) — an upsert, so re-pushing a regenerated summary
            # just replaces it. The Mac owns the LLM; the fleet serves the result.
            store.set_day_summary(body.day, body.text, model=body.model)
        finally:
            store.close()
        return {"ok": True}

    @app.get("/sync/jobs")
    def sync_jobs(
        authorization: str | None = Header(default=None), limit: int = 50
    ) -> list[JobOut]:
        check_token(bearer(authorization), expected)
        store = store_factory()
        try:
            return [_job_of(r) for r in store.pending_refine_requests(limit=limit)]
        finally:
            store.close()

    @app.post("/sync/jobs/{job_id}/done")
    def sync_job_done(
        job_id: int, authorization: str | None = Header(default=None)
    ) -> OkOut:
        check_token(bearer(authorization), expected)
        store = store_factory()
        try:
            store.mark_refine_request_done(job_id)
        finally:
            store.close()
        return {"ok": True}

    return True


class SyncClient:
    """Mac-side client: dials the fleet (never the reverse). Every call carries the
    bearer token and targets the WireGuard address of the host holding the store. The
    httpx client is injectable, so the wire contract is tested against a transport."""

    def __init__(
        self,
        base_url: str,
        token: str,
        *,
        timeout: float = 30.0,
        client: httpx.Client | None = None,
    ) -> None:
        self._base = base_url.rstrip("/")
        self._client = client or httpx.Client(timeout=timeout)
        self._headers = {"Authorization": f"{_BEARER}{token}"}

    def poll_jobs(self, *, limit: int = 50) -> list[JobOut]:
        """Pull pending jobs from the fleet (a cheap reachability check when empty)."""
        resp = self._client.get(
            f"{self._base}/sync/jobs", params={"limit": limit}, headers=self._headers
        )
        resp.raise_for_status()
        return [JobOut.model_validate(job) for job in resp.json()]

    def mark_done(self, job_id: int) -> None:
        """Tell the fleet a job is finished so it isn't handed out again."""
        resp = self._client.post(
            f"{self._base}/sync/jobs/{job_id}/done", headers=self._headers
        )
        resp.raise_for_status()

    def audio_present(self, source: str, name: str) -> bool:
        """Whether the fleet already holds this file — check before uploading."""
        resp = self._client.get(
            f"{self._base}/sync/audio",
            params={"source": source, "name": name},
            headers=self._headers,
        )
        resp.raise_for_status()
        return AudioPresentOut.model_validate(resp.json()).present

    def push_audio(self, source: str, name: str, local_path: Path) -> bool:
        """Upload one archive segment file to the fleet. Idempotent: returns True if the
        fleet stored it, False if it already had it (the immutable archive is never
        overwritten), so the outbox can safely re-push after a failure."""
        with local_path.open("rb") as fh:
            resp = self._client.post(
                f"{self._base}/sync/audio",
                data={"source": source, "name": name},
                files={"file": (name, fh)},
                headers=self._headers,
            )
        resp.raise_for_status()
        return AudioStoredOut.model_validate(resp.json()).stored

    def push_segment(self, segment: SegmentIn) -> SegmentStoredOut:
        """Push a processed segment (metadata + its turns) to the fleet's store.
        First-write-wins, so re-pushing after a failure returns turns_written=0."""
        resp = self._client.post(
            f"{self._base}/sync/segments",
            json=segment.model_dump(),
            headers=self._headers,
        )
        resp.raise_for_status()
        return SegmentStoredOut.model_validate(resp.json())

    def push_summary(self, summary: SummaryIn) -> None:
        """Push a settled day-summary to the fleet (upsert by day, so idempotent)."""
        resp = self._client.post(
            f"{self._base}/sync/summaries",
            json=summary.model_dump(),
            headers=self._headers,
        )
        resp.raise_for_status()
