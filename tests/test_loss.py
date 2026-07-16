"""Reconcile recorded coverage against capture-lifecycle events to find LOST speech.

`active_spans` pairs resume->pause events into the stretches capture was meant to be
recording; `uncovered_loss` flags any portion of those not covered by recorded audio —
capture running but producing nothing — as silently lost, unrecoverable speech.
Coverage-based (not gap-between-segments) so a span with NO segments at all, the
crash-loop shape, is caught too.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import UTC, datetime, timedelta

from recall.capture_control import CaptureEventKind
from recall.loss import active_spans, uncovered_loss


@dataclass(frozen=True)
class _Event:
    """Structurally a store CaptureEvent (only the fields the reconciler reads)."""

    kind: str
    utc: datetime


def _t(hh: int, mm: int = 0, ss: int = 0) -> datetime:
    return datetime(2026, 7, 15, hh, mm, ss, tzinfo=UTC)


NOON = _t(12)


# --- active_spans ---


def test_resume_then_pause_is_one_active_span() -> None:
    events = [
        _Event(CaptureEventKind.RESUME, _t(9)),
        _Event(CaptureEventKind.PAUSE, _t(10)),
    ]
    spans = active_spans(events, now=NOON)
    assert [(s.start, s.end) for s in spans] == [(_t(9), _t(10))]


def test_a_trailing_resume_stays_active_until_now() -> None:
    # Capture resumed and is still running: the active span runs to `now`.
    events = [_Event(CaptureEventKind.RESUME, _t(9))]
    spans = active_spans(events, now=NOON)
    assert [(s.start, s.end) for s in spans] == [(_t(9), NOON)]


def test_events_before_the_first_resume_open_no_span() -> None:
    # A lone pause (or anything before the first resume) gives no active knowledge —
    # we can't claim capture was running, so it contributes nothing (no false loss).
    events = [_Event(CaptureEventKind.PAUSE, _t(9))]
    assert active_spans(events, now=NOON) == []


def test_multiple_cycles_produce_multiple_spans() -> None:
    events = [
        _Event(CaptureEventKind.RESUME, _t(8)),
        _Event(CaptureEventKind.PAUSE, _t(9)),
        _Event(CaptureEventKind.RESUME, _t(10)),
        _Event(CaptureEventKind.PAUSE, _t(11)),
    ]
    spans = active_spans(events, now=NOON)
    assert [(s.start, s.end) for s in spans] == [(_t(8), _t(9)), (_t(10), _t(11))]


def test_events_are_sorted_before_pairing() -> None:
    events = [
        _Event(CaptureEventKind.PAUSE, _t(9)),
        _Event(CaptureEventKind.RESUME, _t(8)),
    ]
    spans = active_spans(events, now=NOON)
    assert [(s.start, s.end) for s in spans] == [(_t(8), _t(9))]


# --- uncovered_loss ---

MIN = timedelta(minutes=1)
SETTLE = timedelta(minutes=10)


def _loss(
    intervals: list[tuple[datetime, datetime]], events: list[_Event]
) -> list[tuple[datetime, datetime]]:
    losses = uncovered_loss(
        intervals, events, "usb", now=NOON, min_loss=MIN, settle=SETTLE
    )
    assert all(g.source_id == "usb" for g in losses)
    return [(g.start, g.end) for g in losses]


def test_a_fully_covered_span_is_no_loss() -> None:
    events = [
        _Event(CaptureEventKind.RESUME, _t(9)),
        _Event(CaptureEventKind.PAUSE, _t(10)),
    ]
    assert _loss([(_t(9), _t(10))], events) == []


def test_a_span_with_no_audio_at_all_is_total_loss() -> None:
    # The crash-loop shape (June): capture "active" for a stretch but not one
    # segment written. There is no gap BETWEEN segments to find — coverage is the
    # only view that sees it. This is the case the old gap-based check missed.
    events = [
        _Event(CaptureEventKind.RESUME, _t(9)),
        _Event(CaptureEventKind.PAUSE, _t(10, 30)),
    ]
    assert _loss([], events) == [(_t(9), _t(10, 30))]


def test_a_hole_in_the_middle_of_a_span_is_loss() -> None:
    events = [
        _Event(CaptureEventKind.RESUME, _t(9)),
        _Event(CaptureEventKind.PAUSE, _t(11)),
    ]
    intervals = [(_t(9), _t(10)), (_t(10, 30), _t(11))]
    assert _loss(intervals, events) == [(_t(10), _t(10, 30))]


def test_an_uncovered_tail_of_a_span_is_loss() -> None:
    # Audio stops arriving mid-span (a wedged producer): the stretch from the last
    # segment to the pause is lost even though no between-gap exists.
    events = [
        _Event(CaptureEventKind.RESUME, _t(9)),
        _Event(CaptureEventKind.PAUSE, _t(11)),
    ]
    assert _loss([(_t(9), _t(10))], events) == [(_t(10), _t(11))]


def test_audio_during_a_pause_is_not_loss_territory() -> None:
    # Coverage is only judged INSIDE active spans; the paused stretch between two
    # spans claims nothing, however empty it is.
    events = [
        _Event(CaptureEventKind.RESUME, _t(8)),
        _Event(CaptureEventKind.PAUSE, _t(9)),
        _Event(CaptureEventKind.RESUME, _t(10, 30)),
        _Event(CaptureEventKind.PAUSE, _t(11)),
    ]
    intervals = [(_t(8), _t(9)), (_t(10, 30), _t(11))]
    assert _loss(intervals, events) == []


def test_boundary_slop_below_min_loss_is_ignored() -> None:
    # First segment starts a beat after the resume; the pause lands a beat after the
    # last segment. Neither sliver is real loss.
    events = [
        _Event(CaptureEventKind.RESUME, _t(9)),
        _Event(CaptureEventKind.PAUSE, _t(10, 0)),
    ]
    intervals = [(_t(9, 0, 30), _t(9, 59, 30))]
    assert _loss(intervals, events) == []


def test_overlapping_intervals_are_merged_not_a_hole() -> None:
    # A long segment fully overlapping a later short one must not read as a hole.
    events = [
        _Event(CaptureEventKind.RESUME, _t(9)),
        _Event(CaptureEventKind.PAUSE, _t(11)),
    ]
    intervals = [(_t(9), _t(11)), (_t(9, 30), _t(10))]
    assert _loss(intervals, events) == []


def test_the_settle_horizon_is_never_judged() -> None:
    # A running capture's newest audio is an in-progress segment plus the indexer's
    # min-age guard: the store ALWAYS trails reality by a few minutes. That trailing
    # stretch must not read as loss.
    events = [_Event(CaptureEventKind.RESUME, _t(9))]  # still running at NOON
    intervals = [(_t(9), NOON - SETTLE)]  # covered right up to the horizon
    assert _loss(intervals, events) == []


def test_loss_ends_at_the_horizon_not_at_now() -> None:
    # Coverage stopped well before the horizon: the loss claim runs to the horizon
    # only — beyond it the indexer may simply not have caught up yet.
    events = [_Event(CaptureEventKind.RESUME, _t(9))]  # still running at NOON
    intervals = [(_t(9), _t(10))]
    assert _loss(intervals, events) == [(_t(10), NOON - SETTLE)]


def test_no_events_means_no_loss_claimed() -> None:
    # Before any events exist (pre-epoch history) we have no pause knowledge, so we
    # never cry loss — the reconciler only judges what it can account for.
    assert _loss([], []) == []
