"""Mac-side job runner — the compute half of the Isis split for on-demand ML.

Isis serves the UI but has no ML and cannot dial the Mac (a one-way WireGuard peer),
so a refine requested from Isis's UI lands only in *Isis's* queue, where the Mac's
daemons never look. The Mac POLLS that queue (`GET /sync/jobs`), hands each job to its
own local refine queue — which the refine daemon drains while the mic is idle — and
marks it done on Isis so it isn't served twice. The refined turns then flow back through
the normal segment/turn sync. Control inverts to a Mac-initiated poll for the same
reason capture_mirror does.

The runner only *bridges* the queue; it runs no ML itself. That keeps heavy refine on
its existing idle-gated daemon (never during recording) and reuses that battle-tested
path, rather than duplicating the ML invocation here.

Pure logic over an injected client + queue, so it is unit-tested with fakes.
"""

from __future__ import annotations

import logging
from collections.abc import Sequence
from datetime import datetime
from typing import Protocol

_log = logging.getLogger("recall.jobs")

# The only job type this Mac knows how to service today. A fleet running ahead of it may
# enqueue others (e.g. ab-compare); those are left pending for a worker that understands
# them rather than acknowledged and lost.
_REFINE = "refine"


class _Job(Protocol):
    """A unit of work pulled from the fleet — structurally `sync.JobOut`."""

    id: int
    type: str
    source: str
    start: str  # ISO-8601
    end: str


class _JobClient(Protocol):
    """The slice of `SyncClient` the runner needs. `Sequence` (not `list`) so a client
    returning `list[JobOut]` satisfies it — list is invariant, Sequence covariant."""

    def poll_jobs(self, *, limit: int = 50) -> Sequence[_Job]: ...
    def mark_done(self, job_id: int) -> None: ...


class _RefineQueue(Protocol):
    """The slice of `Store` the runner needs — the local refine queue."""

    def add_refine_request(
        self, source: str, start: datetime, end: datetime
    ) -> int: ...


def run_jobs_once(queue: _RefineQueue, client: _JobClient, *, limit: int = 50) -> int:
    """Pull pending jobs from the fleet and hand each refine to the local queue. Returns
    how many were handed off.

    Enqueue-then-acknowledge: the job is put in the local queue *before* it's marked
    done on the fleet, so a crash between the two re-serves the job (at worst a harmless
    duplicate — refine just re-supersedes) rather than dropping it. One job failing is
    logged and skipped so it never takes the rest of the batch down with it.
    """
    handed = 0
    for job in client.poll_jobs(limit=limit):
        if job.type != _REFINE:
            _log.warning(
                "leaving job #%s of unknown type %r pending for a capable worker",
                job.id,
                job.type,
            )
            continue
        try:
            queue.add_refine_request(
                job.source,
                datetime.fromisoformat(job.start),
                datetime.fromisoformat(job.end),
            )
            client.mark_done(job.id)
        except Exception:  # one bad job must not drop the others
            _log.exception("job #%s failed to hand off; leaving it pending", job.id)
            continue
        handed += 1
    return handed
