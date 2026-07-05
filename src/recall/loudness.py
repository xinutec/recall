"""Loudness normalisation for playback.

Equality (uniform gain): one gain is applied to the whole clip so its peak hits
near 0 dBFS — playback is loud without manual volume, while the true relative
dynamics are preserved (no per-segment boosting, so background noise isn't pumped
up in the gaps and "who spoke louder/closer" stays faithful). Genuinely-quiet
far-field speech therefore stays only moderately loud — the real fix for that is
better capture (closer/more mics), not a dynamic filter.

The raw recording is never modified; this only shapes the served playback clip.
"""

from __future__ import annotations

import subprocess
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from recall.store import Store, TranscriptSegment

_RMS_RATE = 16000


def speech_level(audio_path: Path, start_s: float, end_s: float) -> float:
    """Peak-ish loudness (0..1) of [start_s, end_s) — a label-ability proxy.

    The 95th-percentile of |amplitude|: captures the *loudest speech moment*,
    not diluted by silence within the turn (mean RMS punishes long turns that are
    mostly gaps). Quiet far-field speech scores low; close, clear speech scores
    high. Used to rank labeling candidates by audio quality.
    """
    import numpy as np  # noqa: PLC0415 - heavy, only for the measure

    start = max(0.0, start_s)
    duration = max(0.0, end_s - start)
    if duration <= 0.0:
        return 0.0
    pcm = subprocess.run(
        [
            "ffmpeg",
            "-hide_banner",
            "-loglevel",
            "error",
            "-ss",
            f"{start:.3f}",
            "-t",
            f"{duration:.3f}",
            "-i",
            str(audio_path),
            "-ac",
            "1",
            "-ar",
            str(_RMS_RATE),
            "-f",
            "s16le",
            "-",
        ],
        capture_output=True,
        check=True,
    ).stdout
    if not pcm:
        return 0.0
    samples = np.frombuffer(pcm, dtype=np.int16).astype(np.float32) / 32768.0
    if samples.size == 0:
        return 0.0
    return float(np.percentile(np.abs(samples), 95))


def _segment_loudness(store: Store, segment: TranscriptSegment) -> float:
    """Measure one turn's loudness from its source audio (a sox/ffmpeg decode)."""
    if segment.audio_segment_id is None:
        return 0.0
    ref = store.audio_segment_ref(segment.audio_segment_id)
    if ref is None:
        return 0.0
    path, audio_start = ref
    start = (segment.start - audio_start).total_seconds()
    end = (segment.end - audio_start).total_seconds()
    try:
        return speech_level(Path(path), start, end)
    except (subprocess.CalledProcessError, OSError):
        return 0.0


def backfill_loudness(store: Store, *, limit: int = 200) -> int:
    """Measure and cache loudness for turns that don't have it yet, off the request
    path. Returns how many were measured. A turn that can't be measured (missing
    audio / decode error) is cached as 0.0 so it isn't retried every pass.
    """
    measured = 0
    for segment in store.segments_missing_loudness(limit=limit):
        store.set_loudness(segment.id, _segment_loudness(store, segment))
        measured += 1
    return measured


def normalize_loudness(src: Path, dst: Path) -> None:
    """Peak-normalise `src` to near 0 dBFS with a single uniform gain (sox norm)."""
    subprocess.run(
        ["sox", str(src), str(dst), "norm", "-1"],
        check=True,
        capture_output=True,
    )
