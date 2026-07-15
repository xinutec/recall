"""Mac-side job runner — bridge the fleet's on-demand refine queue into the local one.

Isis serves the UI but has no ML and cannot dial the Mac (one-way WireGuard), so a
refine requested from Isis's UI lands only in Isis's queue. The Mac polls that queue and
hands each job to its *own* local refine queue, which the refine daemon drains while the
mic is idle; the refined turns then flow back through the normal segment/turn sync.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import override

import pytest

from recall.cli_parser import build_parser
from recall.jobs import run_jobs_once


@dataclass
class _Job:
    id: int
    type: str
    source: str
    start: str
    end: str


class _FakeClient:
    """Stands in for SyncClient's job methods (structurally)."""

    def __init__(self, jobs: list[_Job]) -> None:
        self._jobs = jobs
        self.done: list[int] = []

    def poll_jobs(self, *, limit: int = 50) -> list[_Job]:
        return list(self._jobs)

    def mark_done(self, job_id: int) -> None:
        self.done.append(job_id)


class _FakeQueue:
    """Records local refine enqueues (the slice of Store the runner touches)."""

    def __init__(self) -> None:
        self.added: list[tuple[str, datetime, datetime]] = []

    def add_refine_request(self, source: str, start: datetime, end: datetime) -> int:
        self.added.append((source, start, end))
        return len(self.added)


def _job(
    job_id: int,
    *,
    type: str = "refine",
    source: str = "usb",
    start: str = "2026-07-15T10:00:00+00:00",
    end: str = "2026-07-15T10:05:00+00:00",
) -> _Job:
    return _Job(job_id, type, source, start, end)


def test_a_refine_job_is_handed_to_the_local_queue_then_marked_done() -> None:
    client = _FakeClient([_job(7)])
    queue = _FakeQueue()
    handed = run_jobs_once(queue, client)
    assert handed == 1
    assert queue.added == [
        (
            "usb",
            datetime(2026, 7, 15, 10, 0, tzinfo=UTC),
            datetime(2026, 7, 15, 10, 5, tzinfo=UTC),
        )
    ]
    assert client.done == [7]


def test_no_pending_jobs_is_a_clean_noop() -> None:
    client = _FakeClient([])
    queue = _FakeQueue()
    assert run_jobs_once(queue, client) == 0
    assert queue.added == []
    assert client.done == []


def test_unknown_job_type_is_left_pending_not_run_or_dropped() -> None:
    # A newer fleet may enqueue a type this Mac can't run yet. Skip it (don't wedge the
    # batch) but do NOT mark it done — leave it for a worker that understands it.
    client = _FakeClient([_job(1, type="ab-compare"), _job(2, type="refine")])
    queue = _FakeQueue()
    handed = run_jobs_once(queue, client)
    assert handed == 1
    assert [a[0] for a in queue.added] == ["usb"]  # only the refine was enqueued
    assert client.done == [2]  # the unknown type is not acknowledged


def test_a_failed_enqueue_is_not_marked_done_and_does_not_drop_the_rest() -> None:
    # Enqueue-then-ack ordering: if the local enqueue fails, the fleet must keep the job
    # (re-serve next poll) rather than believe it was handled. One bad job must not skip
    # the others.
    class _OneBadQueue(_FakeQueue):
        @override
        def add_refine_request(
            self, source: str, start: datetime, end: datetime
        ) -> int:
            if source == "boom":
                raise RuntimeError("db locked")
            return super().add_refine_request(source, start, end)

    client = _FakeClient([_job(1, source="boom"), _job(2, source="usb")])
    queue = _OneBadQueue()
    handed = run_jobs_once(queue, client)
    assert handed == 1
    assert [a[0] for a in queue.added] == ["usb"]
    assert client.done == [2]  # job 1 stays pending on the fleet


def test_marks_done_only_after_the_local_enqueue() -> None:
    # The ordering guarantee, made observable: at the moment mark_done runs, the job is
    # already in the local queue.
    order: list[str] = []

    class _OrderQueue(_FakeQueue):
        @override
        def add_refine_request(
            self, source: str, start: datetime, end: datetime
        ) -> int:
            order.append("enqueue")
            return super().add_refine_request(source, start, end)

    class _OrderClient(_FakeClient):
        @override
        def mark_done(self, job_id: int) -> None:
            order.append("done")
            super().mark_done(job_id)

    run_jobs_once(_OrderQueue(), _OrderClient([_job(3)]))
    assert order == ["enqueue", "done"]


def test_parser_wires_jobs() -> None:
    args = build_parser().parse_args(
        ["jobs", "--url", "http://10.100.0.2:8000", "--out", "d"]
    )
    assert args.command == "jobs"
    assert args.url == "http://10.100.0.2:8000"
    assert args.out == Path("d")


def test_parser_requires_a_fleet_url() -> None:
    with pytest.raises(SystemExit):
        build_parser().parse_args(["jobs", "--out", "d"])
