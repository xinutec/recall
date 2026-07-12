"""How *different* a minute sounds from the mic's idle noise: structure, not volume.

Loudness cannot tell a cough from a creak from a word, and the cleanup's whole question
is
what a stretch of audio actually *contains*. But the noise floor has a property loudness
misses: it is stationary. The mic's self-noise has the same spectral shape minute after
minute, hour after hour. Speech, coughs, a chair scraping — anything real — does not.

So each microphone gets a *fingerprint*: the median log-spectral shape of its idle
segments, level-removed, so it describes the colour of its noise rather than its volume.
A segment's `structure` is then how far its loudest moment departs from that shape.

Measured on this archive: idle segments sit at 0.35-0.9, real speech at 0.7-1.4. Note
the
overlap — this ranks, it does not decide. The verdict on speech belongs to the VAD (see
recall.analyse), which is a trained detector and answers the question this cannot.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

import numpy as np

from recall.asr import decode_pcm_f32

SAMPLE_RATE = 8000
# 32 bands over 0-4 kHz. Speech energy lives well inside this, and coarse bands are what
# make the fingerprint stable: a fine spectrum of noise is itself noisy.
BANDS = 32
FRAME = 512
HOP_S = 0.1


def band_shapes(path: Path) -> np.ndarray | None:
    """One level-independent log-spectral shape per HOP_S of a segment, or None if it
    won't decode. The per-frame mean is removed, so the shape says what the sound is
    *made of*, not how loud it was — a whisper and a shout of the same thing match."""
    try:
        pcm = decode_pcm_f32(path, sample_rate=SAMPLE_RATE)
    except (OSError, ValueError, subprocess.CalledProcessError):
        return None
    hop = int(HOP_S * SAMPLE_RATE)
    starts = range(0, len(pcm) - FRAME, hop)
    if len(pcm) < FRAME * 2:
        return None
    frames = np.stack([pcm[i : i + FRAME] for i in starts])
    power = np.abs(np.fft.rfft(frames * np.hanning(FRAME), axis=1)) ** 2 + 1e-12
    edges = np.linspace(0, power.shape[1], BANDS + 1).astype(int)
    bands = np.stack(
        [power[:, edges[i] : edges[i + 1]].mean(axis=1) for i in range(BANDS)], axis=1
    )
    log_bands = np.log10(bands)
    shapes: np.ndarray = log_bands - log_bands.mean(axis=1, keepdims=True)
    return shapes


def fingerprint(idle: list[np.ndarray]) -> np.ndarray:
    """A microphone's noise shape: the median over its idle segments. Median, not mean,
    so one segment less idle than we thought cannot drag the reference toward it."""
    median: np.ndarray = np.median(np.stack([s.mean(axis=0) for s in idle]), axis=0)
    return median


def structure(shapes: np.ndarray, reference: np.ndarray) -> float:
    """How far the *most* unusual moment in a segment departs from the noise shape.

    The maximum, not the mean: a minute holding one two-second cough is a minute with a
    cough in it, and averaging that against 58 seconds of nothing would hide exactly the
    thing worth finding.
    """
    distance = np.sqrt(((shapes - reference) ** 2).mean(axis=1))
    return float(distance.max())


def encode_shape(shape: np.ndarray) -> bytes:
    return np.asarray(shape, dtype=np.float16).tobytes()


def decode_shape(blob: bytes) -> np.ndarray:
    return np.frombuffer(blob, dtype=np.float16).astype(np.float64)
