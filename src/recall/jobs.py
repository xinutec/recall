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

Each job is marked done on Isis only after the local hand-off, so a crash re-serves it
(harmlessly: every step is idempotent) rather than dropping it. Control inverts to a
Mac-initiated poll for the same reason capture_mirror does.

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

from recall.sources import AudioSource, SourceKind
from recall.timeline import Segment

_log = logging.getLogger("recall.jobs")

# The job types this Mac knows how to service. A fleet running ahead of it may enqueue
# others (e.g. ab-compare); those are left pending for a worker that understands them
# rather than acknowledged and lost.
_REFINE = "refine"
_UPLOAD = "upload"


class _Job(Protocol):
    """A unit of work pulled from the fleet — structurally `sync.JobOut`."""

    id: int
    type: str
    source: str
    start: str  # ISO-8601
    end: str
    # Upload-only payload (None on refine jobs / from an older fleet): the blob
    # filename, the source's display title, and the fleet-probed stream shape.
    file: str | None
    title: str | None
    sample_rate: int | None
    channels: int | None


class _JobClient(Protocol):
    """The slice of `SyncClient` the runner needs. `Sequence` (not `list`) so a client
    returning `list[JobOut]` satisfies it — list is invariant, Sequence covariant."""

    def poll_jobs(self, *, limit: int = 50) -> Sequence[_Job]: ...
    def mark_done(self, job_id: int, *, job_type: str = "refine") -> None: ...
    def fetch_audio(self, source: str, name: str, dest: Path) -> None: ...


class _LocalStore(Protocol):
    """The slice of `Store` the runner needs — the local refine queue plus the
    (idempotent, INSERT OR IGNORE) source/segment registration an upload lands in."""

    def add_refine_request(
        self, source: str, start: datetime, end: datetime
    ) -> int: ...
    def add_source(self, source: AudioSource) -> None: ...
    def add_audio_segment(self, segment: Segment) -> int: ...


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
    if not job.file or job.sample_rate is None or job.channels is None:
        raise ValueError(f"upload job #{job.id} is missing its blob metadata")
    dest = data_root / job.source / job.file
    if not dest.exists():
        client.fetch_audio(job.source, job.file, dest)
    store.add_source(
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
                store.add_refine_request(
                    job.source,
                    datetime.fromisoformat(job.start),
                    datetime.fromisoformat(job.end),
                )
            elif job.type == _UPLOAD:
                _pull_upload(store, client, data_root, job)
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
