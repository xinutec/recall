"""Grouping consecutive quiet capture segments into long spans to review for delete."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta

from recall.ids import AudioSegmentId
from recall.quiet import SegmentVolume, find_quiet_spans

BASE = datetime(2026, 7, 11, 9, 0, 0, tzinfo=UTC)


def _seg(i: int, mean_db: float | None, *, seconds: float = 59.0) -> SegmentVolume:
    start = BASE + timedelta(seconds=i * seconds)
    return SegmentVolume(
        audio_id=AudioSegmentId(i),
        start=start,
        end=start + timedelta(seconds=seconds),
        mean_db=mean_db,
    )


def test_long_run_of_quiet_is_one_span() -> None:
    # 10 * 59s = 590s of noise-floor quiet, over the 300s minimum.
    segs = [_seg(i, -62.0) for i in range(10)]
    spans = find_quiet_spans(segs, threshold_db=-60.0, min_duration_s=300.0)
    assert len(spans) == 1
    assert spans[0].audio_ids == tuple(AudioSegmentId(i) for i in range(10))
    assert spans[0].duration_s == 590.0


def test_short_quiet_run_is_ignored() -> None:
    # 3 * 59s = 177s < 300s — normal between-utterance quiet, not a deletable span.
    segs = [_seg(i, -62.0) for i in range(3)]
    assert find_quiet_spans(segs, min_duration_s=300.0) == []


def test_speech_splits_quiet_into_separate_spans() -> None:
    # quiet(6) | speech | quiet(6) → two spans, the loud segment excluded from both.
    segs = (
        [_seg(i, -62.0) for i in range(6)]
        + [_seg(6, -50.0)]  # a real sound breaks the run
        + [_seg(i, -62.0) for i in range(7, 13)]
    )
    spans = find_quiet_spans(segs, threshold_db=-60.0, min_duration_s=300.0)
    assert len(spans) == 2
    assert AudioSegmentId(6) not in spans[0].audio_ids + spans[1].audio_ids


def test_unmeasured_segment_breaks_a_run() -> None:
    # None = couldn't measure; don't sweep an unknown segment into a delete.
    segs = (
        [_seg(i, -62.0) for i in range(6)]
        + [_seg(6, None)]
        + [_seg(i, -62.0) for i in range(7, 13)]
    )
    spans = find_quiet_spans(segs, min_duration_s=300.0)
    assert len(spans) == 2


def test_threshold_is_a_clean_cut() -> None:
    # -60 threshold: -62 quiet in, -55 (quietest real sound) out.
    segs = [_seg(i, -62.0) for i in range(5)] + [_seg(i, -55.0) for i in range(5, 10)]
    spans = find_quiet_spans(segs, threshold_db=-60.0, min_duration_s=200.0)
    assert len(spans) == 1
    assert spans[0].audio_ids == tuple(AudioSegmentId(i) for i in range(5))
