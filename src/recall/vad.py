"""Voice-activity detection: find where there's actually speech.

Whisper hallucinates on silence/near-silence — on an empty room it emits filler
like "Gracias.", "Thank you.", "So" at high confidence. The fix is to gate the
transcriber on a VAD: only transcribe spans a detector is confident contain
speech, and skip the rest. Raw audio is always retained, so gating only the
*transcription* is safe — a future, better VAD can re-derive the dropped spans.

The detector is injected (a `Vad`), so the pipeline is testable with a stub; the
real implementation (Silero VAD) is lazy/heavy and exercised live, not in tests.
"""

from __future__ import annotations

import subprocess
from dataclasses import dataclass
from functools import lru_cache
from pathlib import Path
from typing import Protocol

_SAMPLE_RATE = 16000
# Phone mics capture un-gained, ~25-40 dB below the USB mic — which left their clearly
# audible speech below the detector's level sensitivity, so it was gated to silence and
# dropped even though it transcribes perfectly. Lift a clip's peak toward this target
# before detection, bounded by _DETECT_MAX_GAIN so near-silent room tone isn't amplified
# into a false speech trigger. The ASR still sees the original audio.
_DETECT_TARGET_PEAK = 0.5
_DETECT_MAX_GAIN = 32.0


def _detection_gain(peak: float) -> float:
    """Gain to lift a clip's peak toward the detector's target level — bounded, and
    never attenuating (>= 1.0). Quiet but real speech clears the gate; near-silence,
    capped, stays too low to read as speech."""
    if peak <= 0.0 or peak >= _DETECT_TARGET_PEAK:
        return 1.0
    return min(_DETECT_TARGET_PEAK / peak, _DETECT_MAX_GAIN)


@dataclass(frozen=True)
class SpeechRegion:
    """A span of detected speech, in seconds from the start of the audio file."""

    start: float
    end: float


class Vad(Protocol):
    """Returns the speech regions in an audio file (empty if none)."""

    def __call__(self, audio_path: Path, /) -> list[SpeechRegion]: ...


def overlaps_speech(start: float, end: float, regions: list[SpeechRegion]) -> bool:
    """True if [start, end) overlaps any speech region (all seconds in-file)."""
    return any(r.start < end and r.end > start for r in regions)


def _decode_mono_16k(audio_path: Path) -> bytes:
    """Decode any input to 16 kHz mono s16le PCM (what Silero expects)."""
    proc = subprocess.run(
        [
            "ffmpeg",
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            str(audio_path),
            "-ac",
            "1",
            "-ar",
            str(_SAMPLE_RATE),
            "-f",
            "s16le",
            "-",
        ],
        capture_output=True,
        check=True,
    )
    return proc.stdout


@lru_cache(maxsize=1)
def _model() -> object:
    """The Silero network, loaded once per process.

    It was being loaded on *every call*: ~2s of model construction to run 0.5s of
    detection, so the cleanup's listening pass ran at 2.5s a segment instead of 0.6s —
    five hours instead of one. Every VAD user in the pipeline (worker, ingest, redrive)
    was paying it too, once per clip.
    """
    from silero_vad import load_silero_vad  # noqa: PLC0415

    return load_silero_vad()


def silero_speech_regions(
    audio_path: Path,
    *,
    threshold: float = 0.5,
    min_speech_ms: int = 250,
    min_silence_ms: int = 300,
) -> list[SpeechRegion]:
    """Silero VAD speech regions for `audio_path`.

    `threshold` is the "are we sure it's speech" knob (higher = stricter, fewer
    false speech detections on background noise). Below it, audio is treated as
    non-speech and not transcribed.
    """
    import numpy as np  # noqa: PLC0415 - heavy, only for the real detector
    import torch  # noqa: PLC0415
    from silero_vad import get_speech_timestamps  # noqa: PLC0415

    pcm = _decode_mono_16k(audio_path)
    if not pcm:
        return []
    audio = np.frombuffer(pcm, dtype=np.int16).astype(np.float32) / 32768.0
    # Level-normalise (bounded) before detection, so quiet phone audio isn't gated.
    peak = float(np.abs(audio).max()) if audio.size else 0.0
    audio = audio * _detection_gain(peak)
    stamps = get_speech_timestamps(
        torch.from_numpy(audio),
        _model(),
        sampling_rate=_SAMPLE_RATE,
        threshold=threshold,
        min_speech_duration_ms=min_speech_ms,
        min_silence_duration_ms=min_silence_ms,
    )
    return [
        SpeechRegion(start=s["start"] / _SAMPLE_RATE, end=s["end"] / _SAMPLE_RATE)
        for s in stamps
    ]
