"""A/B model comparison: per-segment text diff + WER against corrections, with
stub transcribers (no ML). Uses real ffmpeg for the working copy + span slice."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from pathlib import Path

from conftest import make_flac
from recall.abcompare import compare_models, render_markdown
from recall.sources import AudioSource, SourceKind
from recall.store import Store
from recall.timeline import Segment

BASE = datetime(2026, 6, 19, 13, 0, 0, tzinfo=UTC)


def _old(_p: Path) -> str:
    return "the quick brown fox"


def _new(_p: Path) -> str:
    return "hello world"


def test_ab_compare_reports_diff_and_wer(tmp_path: Path) -> None:
    flac = tmp_path / "usb-20260619T130000.flac"
    make_flac(flac, 4.0)
    store = Store.memory()
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    audio_id = store.add_audio_segment(
        Segment(
            source_id="usb",
            sequence=0,
            start=BASE,
            end=BASE + timedelta(seconds=4),
            path=str(flac),
            sample_rate=48000,
            channels=1,
        )
    )
    # A human correction (ground truth "hello world") over [+0.5s, +2.5s].
    tid = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=0.5),
        end=BASE + timedelta(seconds=2.5),
        text="helo wrld",
        asr_model="old",
    )
    store.add_correction(
        transcript_segment_id=tid,
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=0.5),
        end=BASE + timedelta(seconds=2.5),
        original_text="helo wrld",
        corrected_text="hello world",
        language="en",
        created=BASE,
    )

    report = compare_models(
        store,
        _old,
        _new,
        audio_ids=[audio_id],
        work_dir=tmp_path / "work",
        model_a="base",
        model_b="adapter",
    )

    # Whole-segment text diff: the two models disagree.
    assert report.n_segments == 1
    assert report.n_changed == 1
    diff = report.segment_diffs[0]
    assert diff.text_a == "the quick brown fox"
    assert diff.text_b == "hello world"

    # WER vs the corrected span: B is exact (0.0), A is wrong (> 0).
    assert len(report.correction_scores) == 1
    score = report.correction_scores[0]
    assert score.wer_b == 0.0
    assert score.wer_a > 0.0
    assert report.mean_wer_b == 0.0
    assert report.mean_wer_a is not None and report.mean_wer_a > report.mean_wer_b

    # The markdown calls the winner, and scratch WAVs self-clean.
    assert "B is better" in render_markdown(report)
    assert list((tmp_path / "work").glob("*.wav")) == []


def test_ab_compare_no_corrections_still_diffs(tmp_path: Path) -> None:
    flac = tmp_path / "usb-20260619T130000.flac"
    make_flac(flac, 4.0)
    store = Store.memory()
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    audio_id = store.add_audio_segment(
        Segment(
            source_id="usb",
            sequence=0,
            start=BASE,
            end=BASE + timedelta(seconds=4),
            path=str(flac),
            sample_rate=48000,
            channels=1,
        )
    )
    report = compare_models(
        store,
        _old,
        _new,
        audio_ids=[audio_id],
        work_dir=tmp_path / "work",
        model_a="base",
        model_b="adapter",
    )
    assert report.n_segments == 1
    assert report.mean_wer_a is None  # no ground truth in range
    assert report.mean_wer_b is None
    assert "WER unknown" in render_markdown(report)
