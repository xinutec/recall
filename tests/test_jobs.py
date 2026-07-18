"""Mac-side job runner — bridge the fleet's on-demand queues into local ones.

Isis serves the UI but has no ML and cannot dial the Mac (one-way WireGuard), so work
born on Isis lands only in Isis's queues. The Mac polls them and brings each job home:
a refine goes to the local refine queue (drained while the mic is idle); an uploaded
session's blob is fetched into the local archive and registered so the worker's normal
pass transcribes it. Results flow back through the normal segment/turn sync.
"""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import override

import pytest

from recall.cli_parser import build_parser
from recall.ids import AudioSegmentId
from recall.jobs import run_jobs_once
from recall.sources import AudioSource, SourceKind
from recall.store_models import AbCompareJob, AskRequestStatus, SweepEvidence
from recall.timeline import Segment


@dataclass
class _Job:
    id: int
    type: str
    source: str
    start: str | None
    end: str | None
    file: str | None = None
    title: str | None = None
    sample_rate: int | None = None
    channels: int | None = None
    model_a: str | None = None
    model_b: str | None = None
    base_model: str | None = None
    status: str | None = None
    prompt: str | None = None


class _FakeClient:
    """Stands in for SyncClient's job methods (structurally)."""

    def __init__(self, jobs: list[_Job]) -> None:
        self._jobs = jobs
        self.done: list[tuple[int, str]] = []
        self.fetched: list[tuple[str, str, Path]] = []
        self.ab_running: list[int] = []
        self.ab_results: list[tuple[int, dict[str, object]]] = []
        self.ask_results: list[tuple[int, dict[str, str | None]]] = []

    def poll_jobs(self, *, limit: int = 50) -> list[_Job]:
        return list(self._jobs)

    def mark_done(self, job_id: int, *, job_type: str = "refine") -> None:
        self.done.append((job_id, job_type))

    def push_ask_result(
        self, request_id: int, *, answer: str | None = None, error: str | None = None
    ) -> None:
        self.ask_results.append((request_id, {"answer": answer, "error": error}))

    def fetch_audio(self, source: str, name: str, dest: Path) -> None:
        self.fetched.append((source, name, dest))
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_bytes(b"opus-bytes")

    def mark_ab_compare_running(self, run_id: int) -> None:
        self.ab_running.append(run_id)

    def push_ab_compare_result(  # noqa: PLR0913 - mirrors the SyncClient signature
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
    ) -> None:
        self.ab_results.append((run_id, {"error": error, "result_json": result_json}))


class _FakeStore:
    """Records the local hand-offs (the slice of Store the runner touches)."""

    def __init__(self) -> None:
        self.refines: list[tuple[str, datetime, datetime]] = []
        self.sources: list[AudioSource] = []
        self.segments: list[Segment] = []
        self.ab_added: list[tuple[str, int | None]] = []
        self.ab_local: dict[int, AbCompareJob] = {}  # fleet_id -> local mirror
        # (source, start) -> the Mac's own evidence about the segment a sweep names
        self.evidence: dict[tuple[str, datetime], SweepEvidence] = {}
        # audio id -> paths its deletion frees
        self.freed: dict[int, list[str]] = {}
        self.deleted: list[list[int]] = []
        self.refusals: list[tuple[str, datetime, str]] = []
        self.ask_added: list[tuple[str, int | None]] = []  # (prompt, fleet_id)
        self.ask_local: dict[int, AskRequestStatus] = {}  # fleet_id -> local status
        self.ask_deleted: list[int] = []  # local ids the relay discarded as stale

    def add_refine_request(self, source: str, start: datetime, end: datetime) -> int:
        self.refines.append((source, start, end))
        return len(self.refines)

    def add_ask_request(
        self,
        question: str,
        prompt: str,
        sources: Sequence[int],
        *,
        fleet_id: int | None = None,
    ) -> int:
        self.ask_added.append((prompt, fleet_id))
        return len(self.ask_added)

    def ask_request_by_fleet_id(self, fleet_id: int) -> AskRequestStatus | None:
        return self.ask_local.get(fleet_id)

    def delete_ask_request(self, request_id: int) -> None:
        self.ask_deleted.append(request_id)
        for fid, st in list(self.ask_local.items()):
            if st.id == request_id:
                del self.ask_local[fid]

    def add_source(self, source: AudioSource) -> None:
        self.sources.append(source)

    def add_audio_segment(self, segment: Segment) -> int:
        self.segments.append(segment)
        return len(self.segments)

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
    ) -> int:
        self.ab_added.append((source, fleet_id))
        return len(self.ab_added)

    def ab_compare_run_by_fleet_id(self, fleet_id: int) -> AbCompareJob | None:
        return self.ab_local.get(fleet_id)

    def sweep_evidence(self, source: str, start: datetime) -> SweepEvidence | None:
        return self.evidence.get((source, start))

    def record_sweep_refusal(self, source: str, start: datetime, reason: str) -> None:
        self.refusals.append((source, start, reason))

    def delete_audio_segments(self, audio_ids: Sequence[AudioSegmentId]) -> list[str]:
        self.deleted.append([int(a) for a in audio_ids])
        paths: list[str] = []
        for audio_id in audio_ids:
            paths.extend(self.freed.get(int(audio_id), []))
        return paths


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
    client = _FakeClient([_job(1, type="score-asr"), _job(2, type="refine")])
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


def _ask_job(job_id: int = 31, *, prompt: str = "PROMPT") -> _Job:
    return _Job(job_id, "ask", "", None, None, prompt=prompt)


def _local_ask(
    *,
    done: bool,
    answer: str | None = None,
    error: str | None = None,
    prompt: str = "PROMPT",  # matches _ask_job's default prompt
) -> AskRequestStatus:
    return AskRequestStatus(
        id=1,
        question="",
        prompt=prompt,
        sources=(),
        answer=answer,
        error=error,
        done=done,
        created=datetime(2026, 7, 18, tzinfo=UTC),
    )


def test_an_unseen_ask_job_is_adopted_locally_and_never_acknowledged(
    tmp_path: Path,
) -> None:
    # Like A/B compare: the ask lands in the local queue (stamped with the fleet's id)
    # for the refine daemon's LLM, and the fleet keeps serving it — only the answer
    # landing retires it, so no mark_done ever fires and generation never happens here.
    client = _FakeClient([_ask_job(31, prompt="Answer this.")])
    store = _FakeStore()
    handed = run_jobs_once(store, client, data_root=tmp_path)
    assert handed == 1
    assert store.ask_added == [("Answer this.", 31)]  # prompt adopted under fleet id 31
    assert client.done == []
    assert client.ask_results == []


def test_an_adopted_ask_job_still_pending_relays_nothing(tmp_path: Path) -> None:
    client = _FakeClient([_ask_job(31)])
    store = _FakeStore()
    store.ask_local[31] = _local_ask(done=False)
    handed = run_jobs_once(store, client, data_root=tmp_path)
    assert handed == 0
    assert store.ask_added == []  # already adopted — not re-added
    assert client.ask_results == []


def test_a_finished_ask_job_pushes_the_answer_back(tmp_path: Path) -> None:
    client = _FakeClient([_ask_job(31)])
    store = _FakeStore()
    store.ask_local[31] = _local_ask(done=True, answer="It was Tuesday.")
    handed = run_jobs_once(store, client, data_root=tmp_path)
    assert handed == 1
    assert client.ask_results == [(31, {"answer": "It was Tuesday.", "error": None})]
    assert client.done == []  # the answer landing retires it, not an acknowledgement


def test_a_failed_ask_job_pushes_the_error_back(tmp_path: Path) -> None:
    client = _FakeClient([_ask_job(31)])
    store = _FakeStore()
    store.ask_local[31] = _local_ask(done=True, error="model failed to load")
    run_jobs_once(store, client, data_root=tmp_path)
    assert client.ask_results == [
        (31, {"answer": None, "error": "model failed to load"})
    ]


def test_a_reused_fleet_id_with_a_new_prompt_discards_the_stale_answer(
    tmp_path: Path,
) -> None:
    # Fleet ask ids can be reused (after a manual row delete). If the adopted local copy
    # for that id belongs to a DIFFERENT question, relaying its answer would return a
    # stale/wrong answer (the "pong for a real question" bug). The relay must detect the
    # prompt mismatch, discard the stale copy, and re-adopt for the real prompt.
    client = _FakeClient([_ask_job(1, prompt="REAL: when did I meet Dr Kosmin?")])
    store = _FakeStore()
    store.ask_local[1] = _local_ask(
        done=True, answer="pong", prompt="STALE synthetic: reply pong"
    )
    handed = run_jobs_once(store, client, data_root=tmp_path)
    assert handed == 1
    assert store.ask_deleted == [1]  # the stale adopted copy was dropped
    assert store.ask_added == [("REAL: when did I meet Dr Kosmin?", 1)]  # re-adopted
    assert client.ask_results == []  # and the stale "pong" was NOT relayed


def _ab_job(job_id: int = 21, *, status: str = "queued") -> _Job:
    return _Job(
        job_id,
        "ab-compare",
        "meeting-20240102-1033",
        None,  # whole recording
        None,
        model_a="mlx-community/whisper-large-v3-turbo",
        model_b="adapter-current",
        base_model="openai/whisper-large-v3",
        status=status,
    )


def _local_run(status: str, *, error: str | None = None) -> AbCompareJob:
    return AbCompareJob(
        id=1,
        source="meeting-20240102-1033",
        start=None,
        end=None,
        model_a="mlx-community/whisper-large-v3-turbo",
        model_b="adapter-current",
        base_model="openai/whisper-large-v3",
        status=status,
        created=datetime(2026, 7, 16, tzinfo=UTC),
        started=None,
        done=None,
        error=error,
        result_json='{"n": 1}' if status == "done" else None,
        mean_wer_a=0.2 if status == "done" else None,
        mean_wer_b=0.25 if status == "done" else None,
        n_corrections=3 if status == "done" else None,
        n_segments=1 if status == "done" else None,
        n_changed=1 if status == "done" else None,
    )


def test_an_unseen_ab_run_is_adopted_locally_and_never_acknowledged(
    tmp_path: Path,
) -> None:
    # Adoption is not completion: the run lands in the local queue (stamped with the
    # fleet's id) for the refine daemon, and the fleet keeps serving it — only the
    # result landing retires it, so no mark_done may ever fire for this type.
    client = _FakeClient([_ab_job(21)])
    store = _FakeStore()
    handed = run_jobs_once(store, client, data_root=tmp_path)
    assert handed == 1
    assert store.ab_added == [("meeting-20240102-1033", 21)]
    assert client.done == []
    assert client.ab_results == []


def test_an_adopted_ab_run_still_pending_locally_relays_nothing(
    tmp_path: Path,
) -> None:
    client = _FakeClient([_ab_job(21)])
    store = _FakeStore()
    store.ab_local[21] = _local_run("queued")
    handed = run_jobs_once(store, client, data_root=tmp_path)
    assert handed == 0
    assert store.ab_added == []  # not adopted twice
    assert client.ab_running == []
    assert client.ab_results == []


def test_a_locally_running_ab_run_is_reported_once(tmp_path: Path) -> None:
    # "Running" is relayed only while the fleet still says queued, so the call
    # happens once, not every 60s pass.
    store = _FakeStore()
    store.ab_local[21] = _local_run("running")
    client = _FakeClient([_ab_job(21, status="queued")])
    assert run_jobs_once(store, client, data_root=tmp_path) == 1
    assert client.ab_running == [21]

    quiet = _FakeClient([_ab_job(21, status="running")])
    assert run_jobs_once(store, quiet, data_root=tmp_path) == 0
    assert quiet.ab_running == []


def test_a_finished_ab_run_pushes_its_report_back(tmp_path: Path) -> None:
    client = _FakeClient([_ab_job(21, status="running")])
    store = _FakeStore()
    store.ab_local[21] = _local_run("done")
    assert run_jobs_once(store, client, data_root=tmp_path) == 1
    ((run_id, pushed),) = client.ab_results
    assert run_id == 21
    assert pushed["result_json"] == '{"n": 1}'
    assert pushed["error"] is None
    assert client.done == []  # retirement is the result itself, not an ack


def test_a_failed_ab_run_pushes_its_error_back(tmp_path: Path) -> None:
    client = _FakeClient([_ab_job(21, status="running")])
    store = _FakeStore()
    store.ab_local[21] = _local_run("error", error="no audio for source")
    assert run_jobs_once(store, client, data_root=tmp_path) == 1
    ((run_id, pushed),) = client.ab_results
    assert run_id == 21
    assert pushed["error"] == "no audio for source"


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


def _sweep_job(job_id: int = 31) -> _Job:
    return _Job(job_id, "sweep", "usb", "2026-07-15T10:00:00+00:00", None)


_SWEEP_AT = datetime(2026, 7, 15, 10, 0, tzinfo=UTC)


def _speechless(audio_id: int = 7) -> SweepEvidence:
    # What the Mac's own review already scored empty — the only thing a sweep may hit.
    return SweepEvidence(
        audio_id=AudioSegmentId(audio_id),
        kind=SourceKind.COREAUDIO,
        speech_s=0.0,
        has_speech=False,
    )


def test_a_sweep_the_mac_scored_speechless_removes_the_local_copy(
    tmp_path: Path,
) -> None:
    # The Mac's own VAD agrees the span is idle capture: the fleet tombstone earns the
    # deletion (rows + file), then is acked. No refusal recorded.
    blob = tmp_path / "usb" / "usb-20260715T100000.opus"
    blob.parent.mkdir(parents=True)
    blob.write_bytes(b"quiet")
    store = _FakeStore()
    store.evidence[("usb", _SWEEP_AT)] = _speechless(7)
    store.freed[7] = [str(blob)]
    client = _FakeClient([_sweep_job(31)])
    assert run_jobs_once(store, client, data_root=tmp_path) == 1
    assert store.deleted == [[7]]
    assert store.refusals == []
    assert not blob.exists()
    assert client.done == [(31, "sweep")]


def test_a_sweep_for_a_segment_not_held_here_is_still_acknowledged(
    tmp_path: Path,
) -> None:
    # Already swept, or never existed locally: the goal state (no copy) holds, so
    # the ack is correct — the fleet must not re-serve it for ever.
    store = _FakeStore()
    client = _FakeClient([_sweep_job(32)])
    assert run_jobs_once(store, client, data_root=tmp_path) == 1
    assert store.deleted == []
    assert store.refusals == []
    assert client.done == [(32, "sweep")]


@pytest.mark.parametrize(
    ("evidence", "why"),
    [
        (
            SweepEvidence(AudioSegmentId(7), SourceKind.UPLOAD, 0.0, False),
            "an uploaded recording is never a sweep target",
        ),
        (
            SweepEvidence(AudioSegmentId(7), SourceKind.COREAUDIO, 0.0, True),
            "a surviving turn means kept speech",
        ),
        (
            SweepEvidence(AudioSegmentId(7), SourceKind.COREAUDIO, None, False),
            "the Mac never measured it speechless",
        ),
        (
            SweepEvidence(AudioSegmentId(7), SourceKind.COREAUDIO, 4.2, False),
            "the Mac's own VAD heard speech",
        ),
    ],
)
def test_a_sweep_the_mac_cannot_justify_is_refused_and_the_audio_kept(
    tmp_path: Path, evidence: SweepEvidence, why: str
) -> None:
    # The Mac is the protected master archive: a fleet (or a compromised Isis) can
    # only command the deletion of audio the Mac itself scored as idle. Every other
    # tombstone is refused — the segment is kept, the refusal journaled — yet still
    # acked so the fleet's own tombstone (which stops re-ingestion) closes the loop
    # without re-serving.
    blob = tmp_path / "usb" / "usb-20260715T100000.opus"
    blob.parent.mkdir(parents=True)
    blob.write_bytes(b"real speech")
    store = _FakeStore()
    store.evidence[("usb", _SWEEP_AT)] = evidence
    store.freed[7] = [str(blob)]
    client = _FakeClient([_sweep_job(33)])
    assert run_jobs_once(store, client, data_root=tmp_path) == 1
    assert store.deleted == []  # nothing destroyed
    assert blob.exists()  # the bytes survive on the Mac
    assert len(store.refusals) == 1
    assert store.refusals[0][0] == "usb"
    assert client.done == [(33, "sweep")], why
