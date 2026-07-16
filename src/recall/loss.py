"""Reconcile recorded coverage against capture-lifecycle events to find LOST speech.

A hole in the always-on mic's coverage is benign only when capture was deliberately
paused across it. The durable pause/resume events (recall.store capture_events) say
when capture was *meant* to be recording: each resume opens an active span, the next
pause closes it. Any part of an active span not covered by recorded audio is capture
running but producing nothing — UNEXPLAINED, i.e. silently lost, unrecoverable speech.
Judged as coverage of the span (not as gaps between segments) so that a span with no
segments at all — the crash-loop shape — is caught too.

Deliberately conservative: with no events (pre-epoch history) or before the first
resume, we make no claim — the reconciler only judges stretches it can account for, so
it never cries loss over an old deliberate pause it has no record of.

Pure logic over events + coverage intervals, so it is unit-tested with fakes.
"""

from __future__ import annotations

from collections.abc import Iterable
from dataclasses import dataclass
from datetime import datetime, timedelta
from typing import Protocol

from recall.capture_control import CaptureEventKind
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
        if event.kind == CaptureEventKind.RESUME:
            if open_start is None:
                open_start = event.utc
        elif event.kind == CaptureEventKind.PAUSE and open_start is not None:
            spans.append(Span(open_start, event.utc))
            open_start = None
    if open_start is not None:
        spans.append(Span(open_start, now))
    return spans


def _merged(
    intervals: Iterable[tuple[datetime, datetime]],
) -> list[tuple[datetime, datetime]]:
    """Coverage intervals sorted and merged (overlaps collapse, running max end)."""
    merged: list[tuple[datetime, datetime]] = []
    for start, end in sorted(intervals):
        if merged and start <= merged[-1][1]:
            merged[-1] = (merged[-1][0], max(merged[-1][1], end))
        else:
            merged.append((start, end))
    return merged


def uncovered_loss(  # noqa: PLR0913 - the reconciliation's tuning knobs are the point
    intervals: Iterable[tuple[datetime, datetime]],
    events: Iterable[_Event],
    source_id: str,
    *,
    now: datetime,
    min_loss: timedelta,
    settle: timedelta,
) -> list[Gap]:
    """Portions of the active spans not covered by any recorded audio — lost speech.

    Coverage-based, not gap-between-segments-based: a span with NO segments at all
    (capture "active" but producing nothing — the crash-loop shape that cost ninety
    minutes in June) leaves no between-segments gap, yet is exactly the total loss
    this check exists for. `min_loss` absorbs boundary slop (a first segment starting
    a beat after the resume, a pause recorded a beat after the last segment; the
    sub-second seams between adjacent segments fall out the same way). `settle`
    excludes the trailing stretch the indexer cannot have caught up with yet — the
    newest audio is an in-progress segment plus the worker's min-age guard, so a
    running capture's last few minutes are always uncovered in the store and must
    never read as loss.
    """
    horizon = now - settle
    merged = _merged(intervals)
    losses: list[Gap] = []
    for span in active_spans(events, now=now):
        end = min(span.end, horizon)
        cursor = span.start
        for start, stop in merged:
            if stop <= cursor:
                continue
            if start >= end:
                break
            if start - cursor >= min_loss:
                losses.append(Gap(source_id=source_id, start=cursor, end=start))
            cursor = max(cursor, stop)
        if end - cursor >= min_loss:
            losses.append(Gap(source_id=source_id, start=cursor, end=end))
    losses.sort(key=lambda g: g.start)
    return losses
