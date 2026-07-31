"""Mac-side job runner — the compute half of the Isis split for on-demand ML.

Isis serves the UI but has no ML and cannot dial the Mac (a one-way WireGuard peer),
so work born on Isis lands only in *Isis's* queues, where the Mac's daemons never look.
The Mac POLLS those queues (`GET /sync/jobs`) and brings each job home:

- **refine** — handed to the local refine queue, which the refine daemon drains while
  the mic is idle; the refined turns then flow back through the normal segment sync.
- **upload** — a session shared to Isis's UI whose audio the Mac has never seen. The
  blob is fetched into this Mac's own archive root (exactly where the retired local
  upload endpoint used to put it) and its source + segment rows are registered, keyed
  to the fleet's start time — so the worker's normal pass transcribes it and the
  pushed-back turns dedupe against the row the fleet already holds.
- **ab-compare** — an A/B run queued on Isis's Compare page. Mirrored into the local
  `ab_compare_runs` queue (stamped with the fleet's run id), where the refine daemon
  executes it; each later pass relays the lifecycle back — "running" once started,
  then the report (or error) itself, which is what retires the run on the fleet.
- **sweep** — a segment deliberately deleted on Isis (a human-confirmed quiet span).
  A sweep is a *request*, not an order: the Mac holds the protected master archive, so
  it applies the deletion only when its OWN evidence agrees the audio is speechless
  idle capture (`speech_s == 0`, no surviving turn, a captured kind). Anything else it
  refuses and keeps, recording the refusal for the doctor. So a compromised Isis can,
  at worst, command the removal of audio the Mac itself already scored empty — never
  real speech, which stays deletable only from this trusted machine.

Refine, upload, and sweep jobs are marked done on Isis only after the local hand-off,
so a crash re-serves them (harmlessly: every step is idempotent) rather than dropping
them; an ab-compare run is retired by its result landing, never by an acknowledgement.
Control inverts to a Mac-initiated poll for the same reason capture_mirror does.

The runner only *bridges* the queues; it runs no ML itself. That keeps heavy work on
the existing idle-gated daemons (never during recording) and reuses those battle-tested
paths, rather than duplicating the ML invocation here.

Pure logic over injected client + store protocols, so it is unit-tested with fakes.
"""

from __future__ import annotations

import logging
from collections.abc import Sequence
from datetime import datetime
from pathlib import Path
from typing import Protocol

from recall.ids import AudioSegmentId
from recall.sources import SWEEPABLE_KINDS, AudioSource, SourceKind
from recall.store_models import AbCompareJob, AskRequestStatus, SweepEvidence
from recall.timeline import Segment

_log = logging.getLogger("recall.jobs")

# The job types this Mac knows how to service. A fleet running ahead of it may enqueue
# others; those are left pending for a worker that understands them rather than
# acknowledged and lost.
_REFINE = "refine"
_UPLOAD = "upload"
_AB_COMPARE = "ab-compare"
_ASK = "ask"
_SWEEP = "sweep"


class _Job(Protocol):
    """A unit of work pulled from the fleet — structurally `sync.JobOut`."""

    id: int
    type: str
    source: str
    start: str | None  # ISO-8601; None = whole recording (ab-compare only)
    end: str | None
    # Upload-only payload (None on other jobs / from an older fleet): the blob
    # filename, the source's display title, and the fleet-probed stream shape.
    file: str | None
    title: str | None
    sample_rate: int | None
    channels: int | None
    # ab-compare-only payload: the two models and the fleet's current run status.
    model_a: str | None
    model_b: str | None
    base_model: str | None
    status: str | None
    # ask-only payload: the self-contained grounded prompt the Mac's LLM answers.
    prompt: str | None


class _JobClient(Protocol):
    """The slice of `SyncClient` the runner needs. `Sequence` (not `list`) so a client
    returning `list[JobOut]` satisfies it — list is invariant, Sequence covariant."""

    def poll_jobs(self, *, limit: int = 50) -> Sequence[_Job]: ...
    def mark_done(self, job_id: int, *, job_type: str = "refine") -> None: ...
    def fetch_audio(self, source: str, name: str, dest: Path) -> None: ...
    def push_ask_result(
        self, request_id: int, *, answer: str | None = None, error: str | None = None
    ) -> None: ...
    def mark_ab_compare_running(self, run_id: int) -> None: ...
    def push_ab_compare_result(  # noqa: PLR0913 - the report's denormalized summary
        self,
        run_id: int,
        *,
        error: str | None = None,
        result_json: str | None = None,
        mean_wer_a: float | None = None,
        mean_wer_b: float | None = None,
        n_corrections: int = 0,
        n_segments: int = 0,
        n_changed: int = 0,
    ) -> None: ...


class _LocalStore(Protocol):
    """The slice of `Store` the runner needs — the local queues plus the idempotent
    source/segment registration an upload lands in. Sources go in via
    `register_source`, not `add_source`: the worker scans the data root continuously
    and can claim a freshly fetched blob's directory first, and only an authoritative
    registration corrects the kind it guessed."""

    def add_refine_request(
        self, source: str, start: datetime, end: datetime
    ) -> int: ...
    def add_ask_request(
        self,
        question: str,
        prompt: str,
        sources: Sequence[int],
        *,
        fleet_id: int | None = None,
    ) -> int: ...
    def ask_request_by_fleet_id(self, fleet_id: int) -> AskRequestStatus | None: ...
    def delete_ask_request(self, request_id: int) -> None: ...
    def add_source(self, source: AudioSource) -> None: ...
    def register_source(self, source: AudioSource) -> None: ...
    def add_audio_segment(self, segment: Segment) -> int: ...
    def add_ab_compare_run(  # noqa: PLR0913 - mirrors the Store signature
        self,
        source: str,
        start: datetime | None,
        end: datetime | None,
        *,
        model_a: str,
        model_b: str,
        base_model: str,
        fleet_id: int | None = None,
    ) -> int: ...
    def ab_compare_run_by_fleet_id(self, fleet_id: int) -> AbCompareJob | None: ...
    def sweep_evidence(self, source: str, start: datetime) -> SweepEvidence | None: ...
    def record_sweep_refusal(
        self, source: str, start: datetime, reason: str
    ) -> None: ...
    def delete_audio_segments(
        self, audio_ids: Sequence[AudioSegmentId]
    ) -> list[str]: ...


def _pull_upload(
    store: _LocalStore, client: _JobClient, data_root: Path, job: _Job
) -> None:
    """Bring one uploaded session home: fetch the blob (skipped if a previous pass
    already did — the archive is immutable) and register its rows. The segment start
    is the fleet's verbatim, so the transcribed turns push back onto the fleet's
    existing row (UNIQUE source_id+start) instead of minting a near-duplicate.

    Crash between fetch and registration is covered without ceremony: the job stays
    pending (not yet acked) and the next 60s pass registers the rows, well inside the
    worker's 120s min-age guard — so the scan never indexes the file first under a
    filename-derived (second-truncated) start."""
    if (
        not job.file
        or job.sample_rate is None
        or job.channels is None
        or job.start is None
        or job.end is None
    ):
        raise ValueError(f"upload job #{job.id} is missing its blob metadata")
    dest = data_root / job.source / job.file
    if not dest.exists():
        client.fetch_audio(job.source, job.file, dest)
    # Authoritative: the blob was just fetched into `data_root/job.source/`, so the
    # worker's directory scan can race this registration and win. Correcting the kind
    # is the difference between a session and a phantom microphone.
    store.register_source(
        AudioSource(
            id=job.source,
            name=job.title or job.source,
            kind=SourceKind.UPLOAD,
            spec="",
        )
    )
    store.add_audio_segment(
        Segment(
            source_id=job.source,
            sequence=0,
            start=datetime.fromisoformat(job.start),
            end=datetime.fromisoformat(job.end),
            path=str(dest),
            sample_rate=job.sample_rate,
            channels=job.channels,
        )
    )


def _bridge_ask(store: _LocalStore, client: _JobClient, job: _Job) -> bool:
    """Advance one fleet ask job by one step; returns whether it moved.

    A relay, like A/B compare: the fleet serves the job until its answer lands. Adopt it
    into the local queue when unseen (the refine daemon — which holds the LLM — drains
    it), then push the answer or error back once the local copy is done; the push-back
    retires it on the fleet. Generation is deliberately NOT done here: `recall jobs` is
    a 60s one-shot and must never load a model."""
    if job.prompt is None:
        raise ValueError(f"ask job #{job.id} is missing its prompt")
    local = store.ask_request_by_fleet_id(job.id)
    if local is not None and local.prompt != job.prompt:
        # This fleet id was reused for a DIFFERENT question — only possible after a
        # manual row deletion (the app never deletes ask jobs), but a stale adopted copy
        # would relay a wrong answer (a "pong" for a real question). Discard it and
        # re-adopt for the real prompt rather than trust the id alone.
        store.delete_ask_request(local.id)
        local = None
    if local is None:
        # Adopt: the question/sources are the fleet's to keep — locally only the prompt
        # (to generate) and the fleet id (to relay back) matter.
        store.add_ask_request("", job.prompt, [], fleet_id=job.id)
        return True
    if not local.done:
        return False  # adopted, awaiting the local refine daemon — nothing to relay yet
    if local.error is not None:
        client.push_ask_result(job.id, error=local.error)
    else:
        client.push_ask_result(job.id, answer=local.answer or "")
    return True


def _bridge_ab_compare(store: _LocalStore, client: _JobClient, job: _Job) -> bool:
    """Advance one fleet A/B run by one lifecycle step; returns whether it moved.

    The fleet serves the run every pass until its result lands, so this is a relay,
    not a hand-off: adopt it into the local queue when unseen (the refine daemon
    executes it), report "running" once the daemon has started (only while the fleet
    still thinks it's queued, so the call happens once), and push the report or error
    when finished — the landing is what retires the run. A fleet row stuck on a Mac
    that lost its local mirror is simply re-adopted on the next pass."""
    local = store.ab_compare_run_by_fleet_id(job.id)
    if local is None:
        if job.model_a is None or job.model_b is None or job.base_model is None:
            raise ValueError(f"ab-compare job #{job.id} is missing its models")
        store.add_ab_compare_run(
            job.source,
            datetime.fromisoformat(job.start) if job.start else None,
            datetime.fromisoformat(job.end) if job.end else None,
            model_a=job.model_a,
            model_b=job.model_b,
            base_model=job.base_model,
            fleet_id=job.id,
        )
        return True
    if local.status == "done":
        client.push_ab_compare_result(
            job.id,
            result_json=local.result_json or "{}",
            mean_wer_a=local.mean_wer_a,
            mean_wer_b=local.mean_wer_b,
            n_corrections=local.n_corrections or 0,
            n_segments=local.n_segments or 0,
            n_changed=local.n_changed or 0,
        )
        return True
    if local.status == "error":
        client.push_ab_compare_result(job.id, error=local.error or "failed")
        return True
    if local.status == "running" and job.status == "queued":
        client.mark_ab_compare_running(job.id)
        return True
    return False  # adopted and awaiting the local daemon — nothing to relay yet


def _sweep_refusal(evidence: SweepEvidence) -> str | None:
    """Why the Mac must NOT honour this sweep, or None if it may. The Mac is the
    protected master archive: a fleet tombstone only earns a deletion when the Mac's
    own evidence agrees the audio is speechless idle capture — the same bar the quiet
    review clears before deleting. So a compromised Isis can, at worst, command the
    removal of audio the Mac itself already scored empty; it can never reach real
    speech, which stays deletable only from the trusted machine."""
    if evidence.kind not in SWEEPABLE_KINDS:
        return f"kind {evidence.kind.value} is never swept (uploaded speech)"
    if evidence.has_speech:
        return "a visible turn stands on it"
    if evidence.speech_s is None:
        return "the Mac has not measured it speechless yet"
    if evidence.speech_s > 0.0:
        return f"the Mac's VAD measured {evidence.speech_s:.1f}s of speech"
    return None


def _apply_sweep(store: _LocalStore, job: _Job) -> None:
    """Apply a fleet tombstone to this Mac's master archive — but only when the Mac's
    own database independently justifies it (see `_sweep_refusal`). Identity is
    (source, start), the same key the push dedupes on.

    Not found locally = already swept or never held; converged. Justified = the same
    hard delete the local quiet review would do (rows + file), which also journals the
    Mac's own tombstone so the deletion is now anchored here too. Refused = the audio
    is KEPT and the refusal recorded for the doctor; a hostile Isis cannot turn a
    sweep into destruction of real speech. Either way the job is acked by the caller:
    Isis's own tombstone stops a refused segment being resurrected by a later push, so
    there's no re-serve loop to keep open."""
    if job.start is None:
        raise ValueError(f"sweep job #{job.id} is missing its identity")
    start = datetime.fromisoformat(job.start)
    evidence = store.sweep_evidence(job.source, start)
    if evidence is None:
        return
    reason = _sweep_refusal(evidence)
    if reason is not None:
        store.record_sweep_refusal(job.source, start, reason)
        _log.warning(
            "sweep REFUSED for %s @ %s (fleet tombstone #%s): %s; keeping the "
            "master copy — real speech is deletable only from this machine",
            job.source,
            job.start,
            job.id,
            reason,
        )
        return
    paths = store.delete_audio_segments([evidence.audio_id])
    for p in paths:
        Path(p).unlink(missing_ok=True)
    _log.info(
        "sweep: removed %s @ %s from the master archive (fleet tombstone #%s)",
        job.source,
        job.start,
        job.id,
    )


def run_jobs_once(
    store: _LocalStore, client: _JobClient, *, data_root: Path, limit: int = 50
) -> int:
    """Pull pending jobs from the fleet and bring each home (see the job types in the
    module docstring). Returns how many were handed off.

    Hand-off-then-acknowledge: the job's local hand-off completes *before* it's marked
    done on the fleet, so a crash between the two re-serves the job (at worst a
    harmless idempotent replay) rather than dropping it. One job failing is logged and
    skipped so it never takes the rest of the batch down with it.
    """
    handed = 0
    for job in client.poll_jobs(limit=limit):
        try:
            if job.type == _REFINE:
                if job.start is None or job.end is None:
                    raise ValueError(f"refine job #{job.id} is missing its window")
                store.add_refine_request(
                    job.source,
                    datetime.fromisoformat(job.start),
                    datetime.fromisoformat(job.end),
                )
            elif job.type == _UPLOAD:
                _pull_upload(store, client, data_root, job)
            elif job.type == _SWEEP:
                _apply_sweep(store, job)
            elif job.type == _ASK:
                # A relay, not a hand-off: the job retires when its answer lands, so
                # there is no mark_done here.
                if _bridge_ask(store, client, job):
                    handed += 1
                continue
            elif job.type == _AB_COMPARE:
                # A relay, not a hand-off: the run retires when its result lands,
                # so there is no mark_done here.
                if _bridge_ab_compare(store, client, job):
                    handed += 1
                continue
            else:
                _log.warning(
                    "leaving job #%s of unknown type %r pending for a capable worker",
                    job.id,
                    job.type,
                )
                continue
            client.mark_done(job.id, job_type=job.type)
        except Exception:  # one bad job must not drop the others
            _log.exception("job #%s failed to hand off; leaving it pending", job.id)
            continue
        handed += 1
    return handed
