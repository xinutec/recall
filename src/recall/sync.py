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
import time
from collections.abc import Callable, Mapping
from datetime import UTC, datetime
from pathlib import Path

import httpx
from fastapi import FastAPI, File, Form, Header, HTTPException, UploadFile
from pydantic import BaseModel

from recall import capture_control
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


class LiveTurnsIn(BaseModel):
    """A batch of provisional live turns the Mac pushes for the fleet's instant feed.
    Audio-less and time-anchored — shown at once, then reconciled (hidden) when the
    archive segment spanning them arrives (see `_ingest_segment`)."""

    turns: list[TurnIn]


class LiveStoredOut(BaseModel):
    """How many pushed live turns the fleet newly stored (present ones are skipped)."""

    stored: int


# /sync/capture long-poll bounds: cap how long the exchange may hang, and re-derive
# the intent every slice while hanging so a pause elapsing on its own (no POST, no
# notify) is still caught mid-hang.
_INTENT_WAIT_CAP_S = 25.0
_INTENT_WAIT_SLICE_S = 2.0


class CaptureAppliedIn(BaseModel):
    """What the Mac currently has applied to its own capture — reported each mirror pass
    so the fleet's status reflects reality, not just what was asked for."""

    running: bool
    pausedUntil: str | None = None  # ISO resume-by, or null when recording
    # Each source's last-proved-recording ISO time (the .alive freshness the Mac
    # owns), so /api/sources on the fleet is truthful. Defaulted: an older Mac client
    # omits it and the fleet just shows no liveness, as before.
    sourceLiveness: dict[str, str] = {}
    # Long-poll: with wait > 0, the exchange hangs (up to `wait` seconds) while the
    # fleet's intent still equals `knownIntent` — the value the Mac last applied
    # (null = running). A press on any UI wakes it in ~RTT, so intent reaches the
    # mic near-instantly while the report cadence stays the mirror's interval.
    # Defaulted: an older Mac short-polls exactly as before.
    wait: float = 0
    knownIntent: str | None = None


class CaptureIntentOut(BaseModel):
    """The fleet's desired capture state: the resume-by time of a pause, or null to run.
    The Mac mirrors this onto its local pause file."""

    pausedUntil: str | None


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


def _capture_exchange(
    store: Store, body: CaptureAppliedIn, now: datetime
) -> CaptureIntentOut:
    """Record the Mac's applied capture state and return the fleet's desired intent —
    the /sync/capture handshake body, pulled out so the route stays a thin wrapper."""
    capture_control.record_reported(
        store,
        running=body.running,
        paused_until=body.pausedUntil,
        now=now,
        source_liveness=body.sourceLiveness,
    )
    # The report just changed what /api/capture serves (confirmed state, freshness):
    # wake hanging client polls so a settle shows in ~RTT of the mirror's confirmation.
    capture_control.notify_capture_changed()
    until = capture_control.intent_until(store, now)
    return CaptureIntentOut(pausedUntil=until.isoformat() if until else None)


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
    # Reconcile the instant feed: this archive segment now covers its span, so any
    # provisional live turns the fleet is still showing inside it are hidden — the
    # fleet-side mirror of worker.reconcile_live, so it swaps live for archive instead
    # of showing both. Runs on every ingest (before the no-op check) so a live turn that
    # arrived after the segment was first stored is still reconciled.
    store.hide_live_turns_covered_by(seg_start, seg_end)
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


def _ingest_live(store: Store, body: LiveTurnsIn) -> LiveStoredOut:
    """Persist pushed live turns on the fleet — audio-less provisional transcripts shown
    instantly while the archive pass catches up. Idempotent: a turn already present (a
    retry, or one the archive already reconciled) is skipped, so a re-push never
    duplicates a turn or resurrects a hidden one."""
    stored = 0
    for turn in body.turns:
        start = datetime.fromisoformat(turn.start)
        if store.live_turn_present(start, turn.text):
            continue
        store.add_transcript_segment(
            audio_segment_id=None,
            start=start,
            end=datetime.fromisoformat(turn.end),
            text=turn.text,
            asr_model=turn.asr_model,
            language=turn.language,
        )
        stored += 1
    return LiveStoredOut(stored=stored)


def _register_live_route(
    app: FastAPI, store_factory: Callable[[], Store], expected: str
) -> None:
    """The instant-feed ingest route (its own helper so register_sync_routes stays under
    the statement budget). The Mac pushes provisional live turns here; they show at once
    and are reconciled when the archive segment spanning them arrives."""

    @app.post("/sync/live")
    def sync_live(
        body: LiveTurnsIn, authorization: str | None = Header(default=None)
    ) -> LiveStoredOut:
        check_token(bearer(authorization), expected)
        store = store_factory()
        try:
            return _ingest_live(store, body)
        finally:
            store.close()


def _register_capture_route(
    app: FastAPI, store_factory: Callable[[], Store], expected: str
) -> None:
    """The capture-control inversion route (its own helper so register_sync_routes stays
    small). The Mac reports its applied state and pulls the fleet's desired intent in
    one round trip; Isis can't dial the one-way peer, so this Mac-initiated exchange is
    how a pause pressed on the fleet's UI reaches the mic. See recall.capture_mirror."""

    @app.post("/sync/capture")
    def sync_capture(
        body: CaptureAppliedIn, authorization: str | None = Header(default=None)
    ) -> CaptureIntentOut:
        check_token(bearer(authorization), expected)
        store = store_factory()
        try:
            reply = _capture_exchange(store, body, datetime.now(UTC))
        finally:
            store.close()
        # Read AFTER the exchange: its own report-notify must not self-wake the
        # hang, while an intent change racing in right here still returns at once.
        seen = capture_control.capture_change_version()
        # Long-poll (see CaptureAppliedIn.wait): the report has landed; now hang
        # while the intent still equals what the Mac already applied. A pause/resume
        # POST wakes the wait (the `seen` version closes the lost-wakeup gap); the
        # slices catch a pause elapsing on its own (intent flips with no POST). The
        # store is reopened per check, never held hanging.
        deadline = time.monotonic() + min(body.wait, _INTENT_WAIT_CAP_S)
        while body.wait > 0 and reply.pausedUntil == body.knownIntent:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            capture_control.wait_capture_changed(
                min(_INTENT_WAIT_SLICE_S, remaining), seen=seen
            )
            seen = capture_control.capture_change_version()
            store = store_factory()
            try:
                until = capture_control.intent_until(store, datetime.now(UTC))
            finally:
                store.close()
            reply = CaptureIntentOut(pausedUntil=until.isoformat() if until else None)
        return reply


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

    _register_live_route(app, store_factory, expected)
    _register_capture_route(app, store_factory, expected)

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

    def push_live(self, turns: list[TurnIn]) -> int:
        """Push a batch of provisional live turns to the fleet's instant feed.
        Idempotent (the fleet skips ones it has); returns how many were newly stored."""
        resp = self._client.post(
            f"{self._base}/sync/live",
            json={"turns": [t.model_dump() for t in turns]},
            headers=self._headers,
        )
        resp.raise_for_status()
        return LiveStoredOut.model_validate(resp.json()).stored

    def exchange_capture(
        self,
        *,
        running: bool,
        paused_until: str | None,
        source_liveness: Mapping[str, str],
        wait: float = 0,
        known_intent: str | None = None,
    ) -> str | None:
        """Report the Mac's applied capture state and receive the fleet's desired intent
        (its resume-by, or None to run). One round trip: push reality, pull intent.
        `source_liveness` carries the sources' .alive freshness the fleet can't see.
        With `wait` > 0 the fleet hangs the reply while its intent still equals
        `known_intent`, so a press comes back in ~RTT (an older fleet ignores both
        and answers immediately, which the mirror paces itself around)."""
        resp = self._client.post(
            f"{self._base}/sync/capture",
            json={
                "running": running,
                "pausedUntil": paused_until,
                "sourceLiveness": dict(source_liveness),
                "wait": wait,
                "knownIntent": known_intent,
            },
            headers=self._headers,
        )
        resp.raise_for_status()
        return CaptureIntentOut.model_validate(resp.json()).pausedUntil
