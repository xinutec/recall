"""Mac-side job runner — bridge the fleet's on-demand queues into local ones.

Isis serves the UI but has no ML and cannot dial the Mac (one-way WireGuard), so work
born on Isis lands only in Isis's queues. The Mac polls them and brings each job home:
a refine goes to the local refine queue (drained while the mic is idle); an uploaded
session's blob is fetched into the local archive and registered so the worker's normal
pass transcribes it. Results flow back through the normal segment/turn sync.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import override

import pytest

from recall.cli_parser import build_parser
from recall.jobs import run_jobs_once
from recall.sources import AudioSource, SourceKind
from recall.timeline import Segment


@dataclass
class _Job:
    id: int
    type: str
    source: str
    start: str
    end: str
    file: str | None = None
    title: str | None = None
    sample_rate: int | None = None
    channels: int | None = None


class _FakeClient:
    """Stands in for SyncClient's job methods (structurally)."""

    def __init__(self, jobs: list[_Job]) -> None:
        self._jobs = jobs
        self.done: list[tuple[int, str]] = []
        self.fetched: list[tuple[str, str, Path]] = []

    def poll_jobs(self, *, limit: int = 50) -> list[_Job]:
        return list(self._jobs)

    def mark_done(self, job_id: int, *, job_type: str = "refine") -> None:
        self.done.append((job_id, job_type))

    def fetch_audio(self, source: str, name: str, dest: Path) -> None:
        self.fetched.append((source, name, dest))
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_bytes(b"opus-bytes")


class _FakeStore:
    """Records the local hand-offs (the slice of Store the runner touches)."""

    def __init__(self) -> None:
        self.refines: list[tuple[str, datetime, datetime]] = []
        self.sources: list[AudioSource] = []
        self.segments: list[Segment] = []

    def add_refine_request(self, source: str, start: datetime, end: datetime) -> int:
        self.refines.append((source, start, end))
        return len(self.refines)

    def add_source(self, source: AudioSource) -> None:
        self.sources.append(source)

    def add_audio_segment(self, segment: Segment) -> int:
        self.segments.append(segment)
        return len(self.segments)


def _job(
    job_id: int,
    *,
    type: str = "refine",
    source: str = "usb",
    start: str = "2026-07-15T10:00:00+00:00",
    end: str = "2026-07-15T10:05:00+00:00",
) -> _Job:
    return _Job(job_id, type, source, start, end)


def _upload_job(job_id: int = 9, *, source: str = "meeting-20260716-1400") -> _Job:
    return _Job(
        job_id,
        "upload",
        source,
        "2026-07-16T13:00:00+00:00",
        "2026-07-16T13:45:00+00:00",
        file=f"{source}-20260716T130000.m4a",
        title="Neurology follow-up",
        sample_rate=48000,
        channels=1,
    )


def test_a_refine_job_is_handed_to_the_local_queue_then_marked_done(
    tmp_path: Path,
) -> None:
    client = _FakeClient([_job(7)])
    store = _FakeStore()
    handed = run_jobs_once(store, client, data_root=tmp_path)
    assert handed == 1
    assert store.refines == [
        (
            "usb",
            datetime(2026, 7, 15, 10, 0, tzinfo=UTC),
            datetime(2026, 7, 15, 10, 5, tzinfo=UTC),
        )
    ]
    assert client.done == [(7, "refine")]


def test_no_pending_jobs_is_a_clean_noop(tmp_path: Path) -> None:
    client = _FakeClient([])
    store = _FakeStore()
    assert run_jobs_once(store, client, data_root=tmp_path) == 0
    assert store.refines == []
    assert client.done == []


def test_an_upload_job_is_fetched_registered_and_marked_done(tmp_path: Path) -> None:
    # The whole point of the upload pull: the blob lands in THIS Mac's archive root,
    # and the segment row carries the fleet's exact start — so the worker transcribes
    # it and the pushed-back turns land on the fleet's existing row, not a duplicate.
    client = _FakeClient([_upload_job(9)])
    store = _FakeStore()
    handed = run_jobs_once(store, client, data_root=tmp_path)
    assert handed == 1
    dest = (
        tmp_path / "meeting-20260716-1400" / "meeting-20260716-1400-20260716T130000.m4a"
    )
    assert dest.read_bytes() == b"opus-bytes"
    assert [s.id for s in store.sources] == ["meeting-20260716-1400"]
    assert store.sources[0].kind == SourceKind.UPLOAD
    assert store.sources[0].name == "Neurology follow-up"
    seg = store.segments[0]
    assert seg.path == str(dest)
    assert seg.start == datetime(2026, 7, 16, 13, 0, tzinfo=UTC)
    assert seg.end == datetime(2026, 7, 16, 13, 45, tzinfo=UTC)
    assert (seg.sample_rate, seg.channels) == (48000, 1)
    assert client.done == [(9, "upload")]


def test_an_upload_already_on_disk_skips_the_fetch_but_still_registers(
    tmp_path: Path,
) -> None:
    # Idempotent replay (a crash after fetch, or a pre-split session that was born on
    # this Mac): the immutable blob is never re-downloaded, the INSERT OR IGNORE row
    # registration re-runs harmlessly, and the fleet still gets its acknowledgement.
    job = _upload_job(4)
    assert job.file is not None
    dest = tmp_path / job.source / job.file
    dest.parent.mkdir(parents=True)
    dest.write_bytes(b"already-here")
    client = _FakeClient([job])
    store = _FakeStore()
    handed = run_jobs_once(store, client, data_root=tmp_path)
    assert handed == 1
    assert client.fetched == []  # never re-downloaded
    assert dest.read_bytes() == b"already-here"
    assert store.segments[0].path == str(dest)
    assert client.done == [(4, "upload")]


def test_an_upload_job_without_blob_metadata_is_left_pending(tmp_path: Path) -> None:
    # A malformed job (no filename / stream shape) can't be brought home; acking it
    # would lose the session. Leave it pending and let the next pass (or a fixed
    # fleet) retry.
    job = _upload_job(5)
    job.file = None
    client = _FakeClient([job, _job(6)])
    store = _FakeStore()
    handed = run_jobs_once(store, client, data_root=tmp_path)
    assert handed == 1  # the refine behind it still went through
    assert client.done == [(6, "refine")]
    assert store.segments == []


def test_unknown_job_type_is_left_pending_not_run_or_dropped(tmp_path: Path) -> None:
    # A newer fleet may enqueue a type this Mac can't run yet. Skip it (don't wedge the
    # batch) but do NOT mark it done — leave it for a worker that understands it.
    client = _FakeClient([_job(1, type="ab-compare"), _job(2, type="refine")])
    store = _FakeStore()
    handed = run_jobs_once(store, client, data_root=tmp_path)
    assert handed == 1
    assert [a[0] for a in store.refines] == ["usb"]  # only the refine was enqueued
    assert client.done == [(2, "refine")]  # the unknown type is not acknowledged


def test_a_failed_enqueue_is_not_marked_done_and_does_not_drop_the_rest(
    tmp_path: Path,
) -> None:
    # Hand-off-then-ack ordering: if the local hand-off fails, the fleet must keep the
    # job (re-serve next poll) rather than believe it was handled. One bad job must not
    # skip the others.
    class _OneBadStore(_FakeStore):
        @override
        def add_refine_request(
            self, source: str, start: datetime, end: datetime
        ) -> int:
            if source == "boom":
                raise RuntimeError("db locked")
            return super().add_refine_request(source, start, end)

    client = _FakeClient([_job(1, source="boom"), _job(2, source="usb")])
    store = _OneBadStore()
    handed = run_jobs_once(store, client, data_root=tmp_path)
    assert handed == 1
    assert [a[0] for a in store.refines] == ["usb"]
    assert client.done == [(2, "refine")]  # job 1 stays pending on the fleet


def test_marks_done_only_after_the_local_enqueue(tmp_path: Path) -> None:
    # The ordering guarantee, made observable: at the moment mark_done runs, the job is
    # already in the local queue.
    order: list[str] = []

    class _OrderStore(_FakeStore):
        @override
        def add_refine_request(
            self, source: str, start: datetime, end: datetime
        ) -> int:
            order.append("enqueue")
            return super().add_refine_request(source, start, end)

    class _OrderClient(_FakeClient):
        @override
        def mark_done(self, job_id: int, *, job_type: str = "refine") -> None:
            order.append("done")
            super().mark_done(job_id, job_type=job_type)

    run_jobs_once(_OrderStore(), _OrderClient([_job(3)]), data_root=tmp_path)
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
