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
# The fallback level at which a bucket counts as a *sound* rather than the mic idling —
# used only for a source too new to have been measured. The real threshold is per
# microphone and derived from that microphone (recall.calibrate): the four recorders
# here differ by 20 dB in both their noise floors and the faintest speech they pick up,
# and one constant applied to all of them hides real words on the quiet ones.
#
# NOT the detector's threshold (quiet.QUIET_MEAN_DB, -60 dB), which judges a 60-second
# *mean*. The noise floor's 0.1s crests cross -60 constantly, so reusing it listed 1,126
# "sounds" in one 100-minute span of dead air, hiding the real ones instead of showing
# them. This value is the USB mic's own measured figure, which is the closest thing to a
# safe default: it is the loudest of the four floors, so it errs toward *more* sounds
# listed on a quieter mic, never fewer.
DEFAULT_EVENT_DB = -52.0

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


@dataclass(frozen=True)
class Measurement:
    """What one decode of a capture segment yields: how loud it was overall, and its
    shape. Both come from the same pass — the scan pays for the decode either way."""

    mean_db: float
    buckets: tuple[float, ...]


def measure(path: Path) -> Measurement | None:
    """Decode a capture segment once and take both its mean volume and its envelope.

    None if it can't be read: a corrupt or vanished file is *unknown*, never silence —
    the archive holds a few, and neither the detector nor the review may treat one as
    empty. `mean_db` is RMS over the whole file (what ffmpeg's volumedetect reports), so
    it stays comparable with volumes measured before envelopes were kept.
    """
    try:
        pcm = decode_pcm_f32(path, sample_rate=DECODE_RATE)
    except (OSError, ValueError, subprocess.CalledProcessError):
        return None
    frames = int(BUCKET_S * DECODE_RATE)
    usable = len(pcm) // frames * frames
    if usable == 0:
        return None
    whole = rms_db(pcm[:usable].reshape(1, -1))
    buckets = rms_db(pcm[:usable].reshape(-1, frames))
    return Measurement(
        mean_db=float(whole[0]), buckets=tuple(float(v) for v in buckets)
    )


# A segment the scan looked at and could not decode: truncated, corrupt, or a stub left
# behind by a dying recorder. Stored as an *empty* envelope — a third state, distinct
# from NULL (never examined) and from a real shape. It matters that it is not NULL: an
# undecodable file would otherwise be re-decoded by every scan, for ever, and the
# archive would never read as fully measured. It stays unquiet all the same: its
# `mean_volume` is left NULL, so `quiet.is_quiet` vetoes it and it is never swept.
UNDECODABLE: bytes = b""


def encode_envelope(buckets: Sequence[float]) -> bytes:
    """Pack an envelope for storage. float16 is ~0.01 dB precise over this range and
    halves the archive's cost to ~11 MB — the drawn shape cannot tell the difference."""
    return np.asarray(buckets, dtype=np.float16).tobytes()


def decode_envelope(blob: bytes) -> tuple[float, ...]:
    """Unpack a stored envelope. UNDECODABLE (empty) unpacks to no buckets at all, which
    the review draws as a gap — an absence of audio, never silence."""
    return tuple(float(v) for v in np.frombuffer(blob, dtype=np.float16))


@lru_cache(maxsize=1024)
def segment_envelope(path: str) -> tuple[float, ...]:
    """dBFS per BUCKET_S of one capture segment by decoding it — the fallback for a
    segment scanned before envelopes were stored, or never scanned at all. Empty if it
    can't be decoded (a gap, never silence). Cached, since panning and zooming the
    review re-read the same segments over and over."""
    measured = measure(Path(path))
    return measured.buckets if measured else ()


@dataclass(frozen=True)
class SpanSound:
    """How much of anything is in a span — what decides whether it is safe to delete.

    `margin_db` is the loudest bucket measured *against that mic's own threshold*, and
    it is the number to rank by. Absolute dB does not compare across recorders (the
    phones' floors sit ~18 dB below the USB mic's), so a span is empty when nothing in
    it rises above what *that* mic calls a sound, not by a global figure. Negative
    means nothing ever did: an empty room.
    """

    loudest_db: float | None
    margin_db: float | None
    sound_seconds: float

    @property
    def silent(self) -> bool:
        """Nothing above this mic's floor, anywhere in it — dead air, and the
        safest thing there is to delete."""
        return self.margin_db is not None and self.margin_db <= 0.0


def summarize_sound(envelopes: Sequence[bytes], threshold_db: float) -> SpanSound:
    """Reduce a span's stored envelopes to how much sound is in it. Pure.

    Reads the buffers straight into numpy rather than through `decode_envelope`.
    Ranking the whole archive touches ~5 million buckets, and materialising each as a
    Python float cost a second on every page load — for numbers only numpy ever sees.
    """
    decoded = [np.frombuffer(e, dtype=np.float16) for e in envelopes if e]
    buckets = np.concatenate(decoded) if decoded else np.zeros(0, dtype=np.float16)
    if not buckets.size:
        return SpanSound(loudest_db=None, margin_db=None, sound_seconds=0.0)
    loudest = float(buckets.max())
    over = int((buckets > threshold_db).sum())
    return SpanSound(
        loudest_db=loudest,
        margin_db=loudest - threshold_db,
        sound_seconds=over * BUCKET_S,
    )


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
