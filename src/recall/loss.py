"""Reconcile archive gaps against capture-lifecycle events to find LOST speech.

A gap in the always-on mic's timeline is benign only when capture was deliberately
paused across it. The durable pause/resume events (recall.store capture_events) say
when capture was *meant* to be recording: each resume opens an active span, the next
pause closes it. Any archive gap overlapping an active span is capture running but
producing no audio — UNEXPLAINED, i.e. silently lost, unrecoverable speech.

Deliberately conservative: with no events (pre-epoch history) or before the first
resume, we make no claim — the reconciler only judges stretches it can account for, so
it never cries loss over an old deliberate pause it has no record of.

Pure logic over events + gaps, so it is unit-tested with fakes.
"""

from __future__ import annotations

from collections.abc import Iterable
from dataclasses import dataclass
from datetime import datetime, timedelta
from typing import Protocol

from recall.capture_control import EVENT_PAUSE, EVENT_RESUME
from recall.timeline import Gap


class _Event(Protocol):
    """The slice of a store CaptureEvent the reconciler reads. Read-only members (a
    property, not a bare annotation) so a frozen dataclass — CaptureEvent, and the test
    fake — structurally satisfies it under strict variance."""

    @property
    def kind(self) -> str: ...
    @property
    def utc(self) -> datetime: ...


@dataclass(frozen=True)
class Span:
    """A stretch capture was meant to be recording (resume -> the next pause)."""

    start: datetime
    end: datetime


def active_spans(events: Iterable[_Event], *, now: datetime) -> list[Span]:
    """The stretches capture was meant to be recording: each resume opens a span, the
    next pause closes it, and a trailing resume stays open to `now`. Events before the
    first resume contribute nothing — we have no basis to call capture active there."""
    spans: list[Span] = []
    open_start: datetime | None = None
    for event in sorted(events, key=lambda e: e.utc):
        if event.kind == EVENT_RESUME:
            if open_start is None:
                open_start = event.utc
        elif event.kind == EVENT_PAUSE and open_start is not None:
            spans.append(Span(open_start, event.utc))
            open_start = None
    if open_start is not None:
        spans.append(Span(open_start, now))
    return spans


def gaps_between(
    intervals: Iterable[tuple[datetime, datetime]],
    source_id: str,
    *,
    tolerance: timedelta,
) -> list[Gap]:
    """Gaps between consecutive coverage intervals (start, end), exceeding `tolerance`.

    Overlaps are merged (running max end), so back-to-back or overlapping segments never
    read as a gap. `tolerance` absorbs the sub-second seam between adjacent segments.
    """
    gaps: list[Gap] = []
    covered_to: datetime | None = None
    for start, end in sorted(intervals):
        if covered_to is not None and start - covered_to > tolerance:
            gaps.append(Gap(source_id=source_id, start=covered_to, end=start))
        covered_to = end if covered_to is None else max(covered_to, end)
    return gaps


def unexplained_loss(
    gaps: Iterable[Gap],
    events: Iterable[_Event],
    *,
    now: datetime,
    min_loss: timedelta,
) -> list[Gap]:
    """Gaps (or the portions of them) that fall while capture was active — lost speech.

    Each gap is intersected with the active spans; an intersection at least `min_loss`
    long is reported (as a Gap trimmed to that stretch). `min_loss` absorbs the boundary
    sliver a pause recorded a few seconds after the last segment would otherwise leave.
    """
    spans = active_spans(events, now=now)
    losses: list[Gap] = []
    for gap in gaps:
        for span in spans:
            lo = max(gap.start, span.start)
            hi = min(gap.end, span.end)
            if hi - lo >= min_loss:
                losses.append(Gap(source_id=gap.source_id, start=lo, end=hi))
    losses.sort(key=lambda g: g.start)
    return losses
