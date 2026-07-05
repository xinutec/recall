"""Shared test fixtures/helpers.

`make_flac` was copy-pasted into ten test files before living here; import it
(`from conftest import make_flac`) rather than re-declaring it.
"""

from __future__ import annotations

import subprocess
from pathlib import Path


def make_flac(path: Path, seconds: float = 3.0) -> None:
    """A real FLAC segment file (440 Hz sine) — enough for probe/slice/embed paths."""
    _encode(path, seconds, "flac")


def make_mp3(path: Path, seconds: float = 3.0) -> None:
    """A real MP3 (libmp3lame) — for the meeting-upload path (uploaded conversations
    are typically mp3), which must probe and store an arbitrary container, not WAV."""
    _encode(path, seconds, "libmp3lame")


def _encode(path: Path, seconds: float, codec: str) -> None:
    subprocess.run(
        [
            "ffmpeg",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            str(seconds),
            "-ac",
            "1",
            "-c:a",
            codec,
            str(path),
        ],
        check=True,
    )
