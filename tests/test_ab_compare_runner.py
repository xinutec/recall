"""The queued-job runner: it marks a run running, then persists the report (with a
denormalized summary) or records the error. The model work is injected, so this is
tested with a hand-built report and no ML."""

from __future__ import annotations

from datetime import UTC, datetime

from recall.abcompare import CorrectionScore, Report, SegmentDiff
from recall.cli import _process_ab_compare_job
from recall.sources import AudioSource, SourceKind
from recall.store import AbCompareJob, Store

BASE = datetime(2026, 6, 22, 9, 33, 0, tzinfo=UTC)


def _source() -> AudioSource:
    return AudioSource(id="usb", name="USB", kind=SourceKind.COREAUDIO, spec="")


def _queued(store: Store) -> AbCompareJob:
    run_id = store.add_ab_compare_run(
        "usb", None, None, model_a="turbo", model_b="adapter", base_model="large-v3"
    )
    job = store.get_ab_compare_run(run_id)
    assert job is not None
    return job


def _report() -> Report:
    return Report(
        model_a="turbo",
        model_b="adapter",
        segment_diffs=[
            SegmentDiff(audio_id=1, start=BASE, text_a="a b c", text_b="a x c")
        ],
        correction_scores=[
            CorrectionScore(
                correction_id=7,
                truth="a b c",
                text_a="a b c",
                text_b="a x c",
                wer_a=0.0,
                wer_b=1 / 3,
            )
        ],
    )


def test_runner_persists_report_and_summary() -> None:
    store = Store.memory()
    store.add_source(_source())
    job = _queued(store)

    report = _report()
    _process_ab_compare_job(store, job, lambda _j: report)

    done = store.get_ab_compare_run(job.id)
    assert done is not None
    assert done.status == "done"
    assert done.n_corrections == 1
    assert done.n_segments == 1
    assert done.n_changed == 1  # the one segment's text differs
    assert done.mean_wer_a == 0.0
    assert done.mean_wer_b == report.mean_wer_b
    # The full report is stored as JSON for the UI to render.
    assert done.result_json is not None
    assert '"correction_id": 7' in done.result_json
    assert store.pending_ab_compare_runs() == []


def test_runner_records_error_without_crashing() -> None:
    store = Store.memory()
    store.add_source(_source())
    job = _queued(store)

    def boom(_j: AbCompareJob) -> Report:
        raise ValueError("no audio for source 'usb' in that range")

    _process_ab_compare_job(store, job, boom)

    failed = store.get_ab_compare_run(job.id)
    assert failed is not None
    assert failed.status == "error"
    assert failed.error is not None
    assert "no audio" in failed.error
    assert failed.result_json is None
    assert store.pending_ab_compare_runs() == []
