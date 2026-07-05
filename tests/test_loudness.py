"""Uniform-gain loudness normalisation for playback."""

from __future__ import annotations

import subprocess
import wave
from pathlib import Path

import numpy as np

from recall.loudness import normalize_loudness


def _quiet_wav(path: Path, amplitude: float = 0.02, seconds: float = 2.0) -> None:
    subprocess.run(
        [
            "ffmpeg",
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            f"sine=frequency=300:duration={seconds}",
            "-af",
            f"volume={amplitude}",
            "-ac",
            "1",
            "-ar",
            "48000",
            str(path),
        ],
        check=True,
    )


def test_normalize_loudness_lifts_quiet_audio(tmp_path: Path) -> None:
    src = tmp_path / "quiet.wav"
    _quiet_wav(src)
    before = src.read_bytes()
    dst = tmp_path / "loud.wav"

    normalize_loudness(src, dst)

    with wave.open(str(dst)) as w:
        data = w.readframes(w.getnframes())
    peak = np.abs(np.frombuffer(data, dtype=np.int16).astype(float) / 32768).max()
    assert peak > 0.8  # lifted to near full scale
    assert src.read_bytes() == before  # source untouched
