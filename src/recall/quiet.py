"""Find long total-quiet spans in the continuous capture — the mic's noise floor, no
speech — so they can be reviewed and deleted (most of the archive is pure waste).

The USB mic emits a consistent noise floor with little variation, so quiet separates
from speech cleanly by RAW mean volume: measured on real capture, quiet segments cluster
near -62 dB (within 1 dB) while any sound sits above ~-55 dB. NB: the DB's `loudness`
column is useless here — it's post-loudnorm, which flattens the gap; the signal is the
raw mean volume of the untouched Opus.

Detection is deliberately human-in-the-loop: this proposes spans; deletion is confirmed
(and boundaries corrected) in the review UI, never automatic.
"""

from __future__ import annotations

import re
import subprocess
from collections.abc import Sequence
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path

from recall.ids import AudioSegmentId
from recall.store import Store

# Between the ~-62 dB noise floor and the ~-55 dB quietest real sound (measured). A
# segment at/under this is the mic idling; above it, something happened.
QUIET_MEAN_DB = -60.0
# Only long runs are worth surfacing — a few seconds of quiet between utterances is
# normal speech rhythm, not waste. Default: 5 minutes.
MIN_QUIET_SPAN_S = 300.0

_MEAN_VOLUME = re.compile(r"mean_volume:\s*(-?\d+(?:\.\d+)?)\s*dB")


@dataclass(frozen=True)
class QuietSpan:
    """A contiguous run of quiet capture segments — a candidate for deletion."""

    start: datetime
    end: datetime
    audio_ids: tuple[AudioSegmentId, ...]

    @property
    def duration_s(self) -> float:
        return (self.end - self.start).total_seconds()


@dataclass(frozen=True)
class SegmentVolume:
    """A capture segment with its raw mean volume (None if it couldn't be measured)."""

    audio_id: AudioSegmentId
    start: datetime
    end: datetime
    mean_db: float | None


def measure_mean_volume(path: Path) -> float | None:
    """The raw mean volume (dBFS) of an audio file via ffmpeg volumedetect, or None if
    it can't be read. Measured on the *untouched* file (no loudnorm) so the noise floor
    stays distinguishable from speech."""
    try:
        out = subprocess.run(
            [
                "ffmpeg",
                "-hide_banner",
                "-i",
                str(path),
                "-af",
                "volumedetect",
                "-f",
                "null",
                "-",
            ],
            capture_output=True,
            text=True,
            check=False,
        )
    except FileNotFoundError:
        return None
    match = _MEAN_VOLUME.search(out.stderr)
    return float(match.group(1)) if match else None


def find_quiet_spans(
    segments: Sequence[SegmentVolume],
    *,
    threshold_db: float = QUIET_MEAN_DB,
    min_duration_s: float = MIN_QUIET_SPAN_S,
) -> list[QuietSpan]:
    """Group consecutive quiet segments (mean volume at/under `threshold_db`) into runs,
    keeping only spans at least `min_duration_s` long. `segments` must be in time order.
    Pure, so the grouping is unit-tested; an unmeasured (None) segment breaks a run
    (unknown, so don't sweep it into a delete)."""
    spans: list[QuietSpan] = []
    run: list[SegmentVolume] = []

    def flush() -> None:
        if run and (run[-1].end - run[0].start).total_seconds() >= min_duration_s:
            spans.append(
                QuietSpan(
                    start=run[0].start,
                    end=run[-1].end,
                    audio_ids=tuple(s.audio_id for s in run),
                )
            )

    for seg in segments:
        if seg.mean_db is not None and seg.mean_db <= threshold_db:
            run.append(seg)
        else:
            flush()
            run = []
    flush()
    return spans


def scan_volumes(store: Store, *, batch: int = 2000) -> int:
    """Measure and cache the raw mean volume of segments not measured yet. ffmpeg per
    file is slow over the whole archive, so it's cached and resumable — this returns how
    many were measured this pass; call again while that's non-zero."""
    measured = 0
    for audio_id, path in store.audio_segments_without_volume(limit=batch):
        mean_db = measure_mean_volume(Path(path))
        if mean_db is not None:
            store.set_audio_mean_volume(audio_id, mean_db)
            measured += 1
    return measured


def quiet_spans(
    store: Store,
    *,
    threshold_db: float = QUIET_MEAN_DB,
    min_duration_s: float = MIN_QUIET_SPAN_S,
) -> list[QuietSpan]:
    """The long total-quiet spans across the archive, from the cached volumes."""
    segments = [
        SegmentVolume(audio_id, start, end, mean_db)
        for audio_id, start, end, mean_db in store.audio_segment_volumes()
    ]
    return find_quiet_spans(
        segments, threshold_db=threshold_db, min_duration_s=min_duration_s
    )
