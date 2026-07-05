"""Recorded-segment timeline: gap and overlap detection.

A *gap* is a hole in a single source's coverage — the worst failure mode for a
memory aid (DESIGN req #1). An *overlap* is two segments of one source covering
the same wall-clock time (expected at segment boundaries; benign but tracked).

All analysis is per-source: sources are independent recorders, so a gap on the
USB mic must never be masked by a phone segment covering the same time.
"""

from __future__ import annotations

from collections import defaultdict
from collections.abc import Iterable
from dataclasses import dataclass
from datetime import datetime, timedelta
from itertools import pairwise
from typing import Final

_ZERO: Final = timedelta(0)


def _require_aware(value: datetime, field: str) -> None:
    if value.tzinfo is None or value.utcoffset() is None:
        msg = f"{field} must be timezone-aware"
        raise ValueError(msg)


@dataclass(frozen=True)
class Segment:
    """One recorded audio file on the timeline, from a single source."""

    source_id: str
    sequence: int
    start: datetime
    end: datetime
    path: str
    sample_rate: int
    channels: int

    def __post_init__(self) -> None:
        _require_aware(self.start, "start")
        _require_aware(self.end, "end")
        if self.end < self.start:
            msg = "segment end is before start"
            raise ValueError(msg)

    @property
    def duration(self) -> timedelta:
        return self.end - self.start


@dataclass(frozen=True)
class Gap:
    """A stretch of wall-clock time with no coverage for `source_id`."""

    source_id: str
    start: datetime
    end: datetime

    @property
    def duration(self) -> timedelta:
        return self.end - self.start


@dataclass(frozen=True)
class Overlap:
    """A stretch of wall-clock time covered by two segments of one source."""

    source_id: str
    start: datetime
    end: datetime

    @property
    def duration(self) -> timedelta:
        return self.end - self.start


def _by_source(segments: Iterable[Segment]) -> dict[str, list[Segment]]:
    grouped: dict[str, list[Segment]] = defaultdict(list)
    for segment in segments:
        grouped[segment.source_id].append(segment)
    for segs in grouped.values():
        segs.sort(key=lambda s: s.start)
    return grouped


def find_gaps(
    segments: Iterable[Segment],
    *,
    tolerance: timedelta = _ZERO,
) -> list[Gap]:
    """Return per-source gaps where the gap exceeds `tolerance`.

    `tolerance` absorbs sub-frame rounding between back-to-back segments; set it
    to zero for strict verification.
    """
    gaps: list[Gap] = []
    for source_id, segs in _by_source(segments).items():
        for current, following in pairwise(segs):
            if following.start - current.end > tolerance:
                gaps.append(
                    Gap(source_id=source_id, start=current.end, end=following.start)
                )
    gaps.sort(key=lambda g: (g.source_id, g.start))
    return gaps


def find_overlaps(
    segments: Iterable[Segment],
    *,
    tolerance: timedelta = _ZERO,
) -> list[Overlap]:
    """Return per-source overlaps where the overlap exceeds `tolerance`."""
    overlaps: list[Overlap] = []
    for source_id, segs in _by_source(segments).items():
        for current, following in pairwise(segs):
            if current.end - following.start > tolerance:
                overlaps.append(
                    Overlap(
                        source_id=source_id,
                        start=following.start,
                        end=min(current.end, following.end),
                    )
                )
    overlaps.sort(key=lambda o: (o.source_id, o.start))
    return overlaps
