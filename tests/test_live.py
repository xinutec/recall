"""Live-transcription helpers (the VAD run loop itself is integration-only)."""

from __future__ import annotations

import wave
from pathlib import Path

from recall.live import mic_argv, write_wav


def test_mic_argv_default_device() -> None:
    argv = mic_argv("")
    assert argv[:2] == ["sox", "-d"]
    assert argv[argv.index("-r") + 1] == "16000"
    assert argv[argv.index("-c") + 1] == "1"
    assert argv[-1] == "-"


def test_mic_argv_named_device_pins_the_mic() -> None:
    # Same pinning as capture: never let a Bluetooth speaker's hands-free mic
    # (the system default input) become the live-transcription source.
    argv = mic_argv("USB Condenser Microphone")
    assert argv[:4] == ["sox", "-t", "coreaudio", "USB Condenser Microphone"]
    assert "-d" not in argv


def test_write_wav_is_valid_mono_16k(tmp_path: Path) -> None:
    pcm = b"\x00\x01" * 16000  # 1 s of 16-bit mono at 16 kHz
    path = tmp_path / "clip.wav"
    write_wav(pcm, path)

    with wave.open(str(path)) as wav:
        assert wav.getnchannels() == 1
        assert wav.getsampwidth() == 2
        assert wav.getframerate() == 16000
        assert wav.getnframes() == 16000
