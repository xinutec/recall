"""Reconcile archive gaps against capture-lifecycle events to find LOST speech.

A gap in the always-on mic's timeline is benign only when capture was paused across it.
`active_spans` pairs resume->pause events into the stretches capture was meant to be
recording; `unexplained_loss` flags any gap overlapping one of those — capture was
running but produced no audio — as silently lost, unrecoverable speech.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import UTC, datetime, timedelta

from recall.capture_control import EVENT_PAUSE, EVENT_RESUME
from recall.loss import active_spans, gaps_between, unexplained_loss
from recall.timeline import Gap


@dataclass(frozen=True)
class _Event:
    """Structurally a store CaptureEvent (only the fields the reconciler reads)."""

    kind: str
    utc: datetime


def _t(hh: int, mm: int = 0) -> datetime:
    return datetime(2026, 7, 15, hh, mm, tzinfo=UTC)


def _gap(start: datetime, end: datetime) -> Gap:
    return Gap(source_id="usb", start=start, end=end)


NOON = _t(12)


# --- active_spans ---


def test_resume_then_pause_is_one_active_span() -> None:
    events = [_Event(EVENT_RESUME, _t(9)), _Event(EVENT_PAUSE, _t(10))]
    spans = active_spans(events, now=NOON)
    assert [(s.start, s.end) for s in spans] == [(_t(9), _t(10))]


def test_a_trailing_resume_stays_active_until_now() -> None:
    # Capture resumed and is still running: the active span runs to `now`.
    events = [_Event(EVENT_RESUME, _t(9))]
    spans = active_spans(events, now=NOON)
    assert [(s.start, s.end) for s in spans] == [(_t(9), NOON)]


def test_events_before_the_first_resume_open_no_span() -> None:
    # A lone pause (or anything before the first resume) gives no active knowledge —
    # we can't claim capture was running, so it contributes nothing (no false loss).
    events = [_Event(EVENT_PAUSE, _t(9))]
    assert active_spans(events, now=NOON) == []


def test_multiple_cycles_produce_multiple_spans() -> None:
    events = [
        _Event(EVENT_RESUME, _t(8)),
        _Event(EVENT_PAUSE, _t(9)),
        _Event(EVENT_RESUME, _t(10)),
        _Event(EVENT_PAUSE, _t(11)),
    ]
    spans = active_spans(events, now=NOON)
    assert [(s.start, s.end) for s in spans] == [(_t(8), _t(9)), (_t(10), _t(11))]


def test_events_are_sorted_before_pairing() -> None:
    events = [_Event(EVENT_PAUSE, _t(9)), _Event(EVENT_RESUME, _t(8))]
    spans = active_spans(events, now=NOON)
    assert [(s.start, s.end) for s in spans] == [(_t(8), _t(9))]


# --- gaps_between ---


def test_contiguous_intervals_have_no_gap() -> None:
    intervals = [(_t(9), _t(10)), (_t(10), _t(11))]
    assert gaps_between(intervals, "usb", tolerance=timedelta(seconds=2)) == []


def test_a_hole_between_intervals_is_a_gap() -> None:
    intervals = [(_t(9), _t(10)), (_t(10, 30), _t(11))]
    gaps = gaps_between(intervals, "usb", tolerance=timedelta(seconds=2))
    assert [(g.start, g.end) for g in gaps] == [(_t(10), _t(10, 30))]


def test_overlapping_intervals_are_merged_not_a_gap() -> None:
    # A long segment fully overlapping a later short one must not read as a gap.
    intervals = [(_t(9), _t(11)), (_t(9, 30), _t(10))]
    assert gaps_between(intervals, "usb", tolerance=timedelta(seconds=2)) == []


# --- unexplained_loss ---


def test_a_gap_inside_a_paused_span_is_not_loss() -> None:
    # paused 9->11 (a resume closes at 9? no: pause at 9, resume at 11 = PAUSED span)
    events = [_Event(EVENT_PAUSE, _t(9)), _Event(EVENT_RESUME, _t(11))]
    gaps = [_gap(_t(9, 30), _t(10, 30))]  # gap sits entirely in the paused stretch
    assert unexplained_loss(gaps, events, now=NOON, min_loss=timedelta(minutes=1)) == []


def test_a_gap_while_capture_was_active_is_loss() -> None:
    # resumed at 9, still running; a 30-min hole in the segments = lost speech.
    events = [_Event(EVENT_RESUME, _t(9))]
    gaps = [_gap(_t(10), _t(10, 30))]
    loss = unexplained_loss(gaps, events, now=NOON, min_loss=timedelta(minutes=1))
    assert [(g.start, g.end) for g in loss] == [(_t(10), _t(10, 30))]


def test_loss_is_trimmed_to_the_active_portion_of_a_gap() -> None:
    # Capture ran 9->10 then paused. A gap 9:40->10:20 is loss only for 9:40->10:00
    # (the part while capture was active); 10:00->10:20 was a legit pause.
    events = [_Event(EVENT_RESUME, _t(9)), _Event(EVENT_PAUSE, _t(10))]
    gaps = [_gap(_t(9, 40), _t(10, 20))]
    loss = unexplained_loss(gaps, events, now=NOON, min_loss=timedelta(minutes=1))
    assert [(g.start, g.end) for g in loss] == [(_t(9, 40), _t(10))]


def test_a_tiny_active_overlap_below_min_loss_is_ignored() -> None:
    # A pause recorded a few seconds after the last segment leaves a sub-threshold
    # sliver of "active gap" that is just the boundary, not real loss.
    events = [_Event(EVENT_RESUME, _t(9)), _Event(EVENT_PAUSE, _t(10, 0))]
    gaps = [_gap(_t(9, 59), _t(10, 30))]  # only 9:59->10:00 is active = 1 min
    loss = unexplained_loss(gaps, events, now=NOON, min_loss=timedelta(minutes=5))
    assert loss == []


def test_no_events_means_no_loss_claimed() -> None:
    # Before any events exist (pre-epoch history) we have no pause knowledge, so we
    # never cry loss — the reconciler only judges what it can account for.
    gaps = [_gap(_t(9), _t(11))]
    assert unexplained_loss(gaps, [], now=NOON, min_loss=timedelta(minutes=1)) == []
