"""Timeline integrity: detecting gaps and overlaps across recorded segments.

This is the core of Phase 0's "prove zero-gap recording" requirement — the
worst failure mode (req #1) is a silent hole in the record, so it gets the
heaviest tests.
"""

from __future__ import annotations

from datetime import UTC, datetime, timedelta

import pytest

from recall.timeline import (
    Gap,
    Overlap,
    Segment,
    find_gaps,
    find_overlaps,
)

BASE = datetime(2026, 6, 13, 14, 0, 0, tzinfo=UTC)


def seg(
    *,
    source: str = "usb",
    sequence: int = 0,
    start_s: float = 0.0,
    dur_s: float = 60.0,
) -> Segment:
    start = BASE + timedelta(seconds=start_s)
    return Segment(
        source_id=source,
        sequence=sequence,
        start=start,
        end=start + timedelta(seconds=dur_s),
        path=f"{source}-{sequence}.wav",
        sample_rate=48000,
        channels=1,
    )


def test_duration() -> None:
    assert seg(dur_s=60).duration == timedelta(seconds=60)


def test_rejects_naive_datetime() -> None:
    naive = datetime(2026, 6, 13, 14, 0, 0)
    with pytest.raises(ValueError, match="timezone-aware"):
        Segment(
            source_id="usb",
            sequence=0,
            start=naive,
            end=naive + timedelta(seconds=60),
            path="x.wav",
            sample_rate=48000,
            channels=1,
        )


def test_rejects_end_before_start() -> None:
    with pytest.raises(ValueError, match=r"end.*before.*start"):
        Segment(
            source_id="usb",
            sequence=0,
            start=BASE,
            end=BASE - timedelta(seconds=1),
            path="x.wav",
            sample_rate=48000,
            channels=1,
        )


def test_no_gaps_when_empty() -> None:
    assert find_gaps([]) == []


def test_no_gaps_single_segment() -> None:
    assert find_gaps([seg()]) == []


def test_contiguous_segments_have_no_gap() -> None:
    segs = [seg(sequence=0, start_s=0, dur_s=60), seg(sequence=1, start_s=60, dur_s=60)]
    assert find_gaps(segs) == []


def test_detects_a_gap() -> None:
    # segment 0 ends at t=60, segment 1 starts at t=65 -> 5s hole
    segs = [seg(sequence=0, start_s=0, dur_s=60), seg(sequence=1, start_s=65, dur_s=60)]
    gaps = find_gaps(segs)
    assert gaps == [
        Gap(
            source_id="usb",
            start=BASE + timedelta(seconds=60),
            end=BASE + timedelta(seconds=65),
        )
    ]
    assert gaps[0].duration == timedelta(seconds=5)


def test_small_gap_within_tolerance_is_ignored() -> None:
    # 20ms sub-frame rounding gap should not be flagged with a 100ms tolerance
    segs = [
        seg(sequence=0, start_s=0, dur_s=60),
        seg(sequence=1, start_s=60.02, dur_s=60),
    ]
    assert find_gaps(segs, tolerance=timedelta(milliseconds=100)) == []
    # ...but with strict (zero) tolerance it is a gap
    assert len(find_gaps(segs)) == 1


def test_overlap_is_not_reported_as_gap() -> None:
    segs = [seg(sequence=0, start_s=0, dur_s=60), seg(sequence=1, start_s=55, dur_s=60)]
    assert find_gaps(segs) == []


def test_find_overlaps() -> None:
    segs = [seg(sequence=0, start_s=0, dur_s=60), seg(sequence=1, start_s=55, dur_s=60)]
    overlaps = find_overlaps(segs)
    assert overlaps == [
        Overlap(
            source_id="usb",
            start=BASE + timedelta(seconds=55),
            end=BASE + timedelta(seconds=60),
        )
    ]
    assert overlaps[0].duration == timedelta(seconds=5)


def test_sources_are_analysed_independently() -> None:
    # a gap on 'usb' must not be masked by 'phone' segments filling that time
    segs = [
        seg(source="usb", sequence=0, start_s=0, dur_s=60),
        seg(source="usb", sequence=1, start_s=120, dur_s=60),  # 60s usb gap
        seg(source="phone", sequence=0, start_s=0, dur_s=180),  # phone covers it
    ]
    gaps = find_gaps(segs)
    assert gaps == [
        Gap(
            source_id="usb",
            start=BASE + timedelta(seconds=60),
            end=BASE + timedelta(seconds=120),
        )
    ]


def test_unsorted_input_is_handled() -> None:
    segs = [seg(sequence=1, start_s=60, dur_s=60), seg(sequence=0, start_s=0, dur_s=60)]
    assert find_gaps(segs) == []
