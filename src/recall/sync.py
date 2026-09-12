"""Mac→fleet sync — the Mac's client half of the Isis/Mac split.

See `docs/isis-migration.md`. The Mac is a one-way WireGuard peer: it may dial the
fleet, nothing may dial back. So every exchange is **Mac-initiated** — the Mac POLLS the
fleet for jobs (it has the ML) and PUSHES results to the fleet's system of record. This
module is the transport for that inversion: job poll (refine + uploaded-session pulls),
audio-blob push and fetch, segment/turns push, live-turn push, and the capture exchange.

⚠ **The other end of every call here is `recalld`, not this file.** The serving half
lived alongside the client until 2026-09-12 — the same pydantic models, registered on a
FastAPI app — and `recalld/src/sync.rs` now answers all of it. Keeping both would mean
two implementations of one wire contract with nothing holding them level, so the Python
routes went and the models stayed. What the models describe is therefore what the RUST
side accepts: change one and `recalld/tests/` is where the disagreement shows up.

The httpx client is injectable, so the wire contract is testable against a transport.
"""

from __future__ import annotations

from collections.abc import Mapping
from pathlib import Path

import httpx
from pydantic import BaseModel

SYNC_TOKEN_ENV = "RECALL_SYNC_TOKEN"
_BEARER = "Bearer "


class JobOut(BaseModel):
    """A unit of work the fleet hands the Mac worker (the Mac has the ML + the mic).

    `type="refine"`: id is a refine-request id. `type="upload"`: id is the fleet's
    audio-segment id, and the upload-only fields carry what the Mac needs to bring the
    session home — the blob filename (fetch via /sync/audio/file), the source's display
    title, and the probed stream shape (so no re-probe). `type="ab-compare"`: id is the
    fleet's run id, start/end are None for a whole-recording run, and the ab-only
    fields carry the two models plus the fleet's current status (so the Mac only
    reports "running" once). There is no sweep type: a quiet-review deletion removes
    the fleet's own copy and leaves a tombstone that refuses a re-push, but it never
    asks a recorder to destroy anything (docs/architecture.md, "Deletion authority").
    Fields outside a job's type are None, and an older fleet simply never sends
    them."""

    id: int
    type: str
    source: str
    start: str | None = None  # ISO-8601
    end: str | None = None
    file: str | None = None
    title: str | None = None
    sample_rate: int | None = None
    channels: int | None = None


class AudioStoredOut(BaseModel):
    """Whether the fleet newly stored the pushed segment. False = it already had it."""

    stored: bool


class AudioPresentOut(BaseModel):
    """Whether the fleet already holds this file — lets the Mac skip re-sending the
    (immutable) blob's bytes on every sync pass."""

    present: bool


class LabelOut(BaseModel):
    """One human voice-naming the Mac pulls back from the fleet: '(source, cluster) is
    <name>'. The UI lives on the fleet, so human naming happens there; this is the only
    way it reaches the Mac's master archive and re-feeds voiceprint enrolment."""

    source_id: str
    cluster: str
    name: str


class TurnIn(BaseModel):
    """One transcript turn the Mac computed for a segment, for the fleet's store."""

    start: str  # ISO-8601
    end: str
    text: str
    asr_model: str
    language: str | None = None
    asr_confidence: float | None = None
    speaker_cluster: str | None = None
    # The Mac's voiceprint guess for this turn (name + cosine strength). The Mac owns
    # the ML; the fleet has none, so unless the guess rides along the push, Isis's UI
    # can only show 'unknown' for freshly-pushed audio. Display-only — speaker_label
    # (the human name) stays authoritative and travels the other way (GET /sync/labels).
    speaker_guess: str | None = None
    speaker_score: float | None = None
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
    """The fleet's audio-segment id, and how many turns it wrote (0 = already had).
    `tombstoned` = the fleet refused the push because this identity was deliberately
    deleted here (audio_segment_id is 0 then) — the Mac must not retry."""

    audio_segment_id: int
    turns_written: int


class LiveStoredOut(BaseModel):
    """How many pushed live turns the fleet newly stored (present ones are skipped)."""

    stored: int


class CaptureIntentOut(BaseModel):
    """The fleet's desired capture state: the resume-by time of a pause, or null to run.
    The Mac mirrors this onto its local pause file."""

    pausedUntil: str | None


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
        # Whether the fleet speaks /sync/segments/batch; flipped off on the first
        # 404/405 so an older fleet costs one probe, not one per flush.
        self._batch_ok = True

    def poll_jobs(self, *, limit: int = 50) -> list[JobOut]:
        """Pull pending jobs from the fleet (a cheap reachability check when empty)."""
        resp = self._client.get(
            f"{self._base}/sync/jobs", params={"limit": limit}, headers=self._headers
        )
        resp.raise_for_status()
        return [JobOut.model_validate(job) for job in resp.json()]

    def fetch_labels(self) -> list[LabelOut]:
        """Pull the fleet's human voice-namings (the whole set) so the Mac can replay
        them onto its own archive. The reverse of every other call here — the fleet is
        authoritative for human input, the Mac for ML — but still Mac-initiated."""
        resp = self._client.get(f"{self._base}/sync/labels", headers=self._headers)
        resp.raise_for_status()
        return [LabelOut.model_validate(lbl) for lbl in resp.json()]

    def mark_done(self, job_id: int, *, job_type: str = "refine") -> None:
        """Tell the fleet a job is finished so it isn't handed out again. `job_type`
        names the id space (see JobOut); an older fleet ignores the parameter, which is
        harmless because it only serves refine jobs in the first place."""
        resp = self._client.post(
            f"{self._base}/sync/jobs/{job_id}/done",
            params={"type": job_type},
            headers=self._headers,
        )
        resp.raise_for_status()

    def fetch_audio(self, source: str, name: str, dest: Path) -> None:
        """Download one audio blob the fleet holds into `dest` (an uploaded session the
        Mac must transcribe). Streamed through a dot-file then renamed into place, so a
        torn download never leaves a plausible-looking partial file — and the dot name
        is invisible to the worker's segment glob (`{source}-*`)."""
        dest.parent.mkdir(parents=True, exist_ok=True)
        part = dest.parent / f".{dest.name}.part"
        with self._client.stream(
            "GET",
            f"{self._base}/sync/audio/file",
            params={"source": source, "name": name},
            headers=self._headers,
        ) as resp:
            resp.raise_for_status()
            with part.open("wb") as fh:
                for chunk in resp.iter_bytes():
                    fh.write(chunk)
        part.replace(dest)

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

    def push_segments(self, segments: list[SegmentIn]) -> list[SegmentStoredOut]:
        """Push a batch of processed segments in one round-trip; falls back to the
        per-segment route (and remembers) against an older fleet that predates
        /sync/segments/batch — additive, so neither side needs the other first."""
        if not segments:
            return []
        if self._batch_ok:
            resp = self._client.post(
                f"{self._base}/sync/segments/batch",
                json={"segments": [s.model_dump() for s in segments]},
                headers=self._headers,
            )
            if resp.status_code not in (404, 405):
                resp.raise_for_status()
                return [
                    SegmentStoredOut.model_validate(r) for r in resp.json()["results"]
                ]
            self._batch_ok = False  # an older fleet; don't re-probe every pass
        return [self.push_segment(s) for s in segments]

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
