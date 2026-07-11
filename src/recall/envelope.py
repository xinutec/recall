"""Amplitude envelope over a window of capture — the waveform the cleanup review draws.

The cached per-segment `mean_volume` is one number per minute: enough to *detect* a
quiet span, far too coarse to *explain* one. To judge a span you have to see its edges —
what broke the silence (a door, a cough, someone walking in and talking) — and whether
the run really is dead air all the way through, so this decodes the Opus and reduces it
to one dB value per bucket.

Two decisions matter for trust, since the output is what a deletion gets approved from:

* The reduction is a *peak* (max over the finer buckets), never a mean. Zooming out must
  never hide a short event: a two-second cough inside a 30-minute view still draws a
  spike. Under-drawing sound here would be the one unacceptable error.
* Gaps — capture paused, segments already deleted — are None, not zero. An absence of
  audio is not silence, and the UI draws it as a gap rather than as quiet.
"""

from __future__ import annotations

import math
import subprocess
from collections.abc import Callable, Sequence
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
from datetime import datetime, timedelta
from functools import lru_cache
from pathlib import Path

import numpy as np

from recall.asr import decode_pcm_f32
from recall.ids import AudioSegmentId

# Envelope resolution: fine enough to resolve a word or a door closing. The API
# down-samples from this to whatever the requested window needs.
BUCKET_S = 0.1
# The envelope needs amplitude, not intelligibility, so decode at 8 kHz mono — ~10x
# cheaper than full rate, with no effect on the drawn shape.
DECODE_RATE = 8000
# Digital silence has no dB; clamp so it plots at the floor instead of -inf.
SILENCE_DB = -100.0
# ffmpeg decodes run in subprocesses (GIL-free), so a window's segments decode at once.
_DECODE_WORKERS = 8
# Sounds closer together than this are one event — two syllables of a word, or a door
# and its latch, should not arrive as separate things for a human to check.
JOIN_GAP_S = 0.5

EnvelopeOf = Callable[[str], Sequence[float]]


@dataclass(frozen=True)
class EnvelopeSegment:
    """A capture segment covering part of an envelope window."""

    audio_id: AudioSegmentId
    path: str
    start: datetime
    end: datetime
    mean_db: float | None


@dataclass(frozen=True)
class SoundEvent:
    """A run of buckets above the quiet threshold — something audible happened here."""

    start: datetime
    end: datetime
    peak_db: float


@dataclass(frozen=True)
class Envelope:
    """A window of capture as one dB value per `bucket_s` (None where no audio exists),
    the segments it was built from (so the UI can play and delete by segment), and every
    audible event in it (so nothing has to be found by eye)."""

    start: datetime
    end: datetime
    bucket_s: float
    points: tuple[float | None, ...]
    segments: tuple[EnvelopeSegment, ...]
    events: tuple[SoundEvent, ...]


def rms_db(buckets: np.ndarray) -> np.ndarray:
    """dBFS of each row of `buckets` (shape (n, frames)), floored at SILENCE_DB."""
    rms = np.sqrt(np.mean(np.square(buckets), axis=1))
    with np.errstate(divide="ignore"):
        db = 20.0 * np.log10(rms)
    floored: np.ndarray = np.maximum(np.nan_to_num(db, neginf=SILENCE_DB), SILENCE_DB)
    return floored


@lru_cache(maxsize=1024)
def segment_envelope(path: str) -> tuple[float, ...]:
    """dBFS per BUCKET_S of one capture segment, empty if it can't be decoded.

    A corrupt or vanished file reads as a gap, never as silence — the archive holds a
    few, and one of them must not take down the review of everything around it. Cached:
    the files are immutable, and panning or zooming re-reads the same segments over.
    """
    try:
        pcm = decode_pcm_f32(Path(path), sample_rate=DECODE_RATE)
    except (OSError, ValueError, subprocess.CalledProcessError):
        return ()
    frames = int(BUCKET_S * DECODE_RATE)
    usable = len(pcm) // frames * frames
    if usable == 0:
        return ()
    return tuple(float(v) for v in rms_db(pcm[:usable].reshape(-1, frames)))


def peak_pool(fine: Sequence[float | None], factor: int) -> list[float | None]:
    """Reduce a fine grid to one value per `factor` buckets, by peak (None = no audio).
    Peak, not mean, so zooming out can never hide a short sound."""
    if factor <= 1:
        return list(fine)
    out: list[float | None] = []
    for i in range(0, len(fine), factor):
        window = [v for v in fine[i : i + factor] if v is not None]
        out.append(max(window) if window else None)
    return out


def find_events(
    points: Sequence[float | None],
    *,
    start: datetime,
    bucket_s: float,
    threshold_db: float,
    join_gap_s: float = JOIN_GAP_S,
) -> list[SoundEvent]:
    """Every run of buckets above `threshold_db` — each audible thing in the window.

    A quiet span is only quiet on a 60-second *mean*: a one-second bump or a cough
    leaves it well under the bar, so a span offered for deletion routinely holds a
    handful of real sounds. Listing them is what makes the review honest — the
    alternative is asking someone to spot a 0.7-second spike in a 15-minute picture,
    which is not reviewing.

    Runs separated by less than `join_gap_s` are one event: two syllables of the same
    word shouldn't arrive as two things to check.
    """
    events: list[SoundEvent] = []
    run_start: int | None = None
    peak = threshold_db
    join_buckets = max(int(join_gap_s / bucket_s), 1)
    quiet_for = 0

    def at(index: int) -> datetime:
        return start + timedelta(seconds=index * bucket_s)

    def flush(end_index: int) -> None:
        if run_start is not None:
            events.append(
                SoundEvent(start=at(run_start), end=at(end_index), peak_db=peak)
            )

    for i, db in enumerate(points):
        if db is not None and db > threshold_db:
            if run_start is None:
                run_start, peak = i, db
            peak = max(peak, db)
            quiet_for = 0
            continue
        if run_start is None:
            continue
        quiet_for += 1
        if quiet_for > join_buckets:
            flush(i - quiet_for + 1)
            run_start, quiet_for = None, 0
    flush(len(points))
    return events


def build_envelope(  # noqa: PLR0913 - a window is defined by all of these
    segments: Sequence[EnvelopeSegment],
    *,
    start: datetime,
    end: datetime,
    threshold_db: float,
    max_points: int = 1500,
    envelope_of: EnvelopeOf | None = None,
) -> Envelope:
    """The envelope of `segments` over [start, end), at the coarsest bucket that still
    yields up to `max_points` values. Segments are placed by *time*, so a pause in
    capture leaves a real gap rather than silently closing up. `envelope_of` is injected
    by tests to stay off ffmpeg; resolved at call time, not bound as a default, so the
    module function stays patchable."""
    lookup = envelope_of if envelope_of is not None else segment_envelope
    span_s = max((end - start).total_seconds(), BUCKET_S)
    n_fine = math.ceil(span_s / BUCKET_S)
    factor = max(1, math.ceil(n_fine / max_points))

    fine: list[float | None] = [None] * n_fine
    with ThreadPoolExecutor(max_workers=_DECODE_WORKERS) as pool:
        measured = list(pool.map(lambda s: lookup(s.path), segments))

    for segment, values in zip(segments, measured, strict=True):
        offset = round((segment.start - start).total_seconds() / BUCKET_S)
        for i, value in enumerate(values):
            index = offset + i
            if 0 <= index < n_fine:
                fine[index] = value

    # Events come off the *fine* grid, not the pooled one: what the reviewer is asked to
    # listen to must not depend on how far they happened to be zoomed out.
    return Envelope(
        start=start,
        end=end,
        bucket_s=BUCKET_S * factor,
        points=tuple(peak_pool(fine, factor)),
        segments=tuple(segments),
        events=tuple(
            find_events(fine, start=start, bucket_s=BUCKET_S, threshold_db=threshold_db)
        ),
    )
