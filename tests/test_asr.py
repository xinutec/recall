"""The ASR result types, the slice and decode helpers, and the sampler fix."""

from __future__ import annotations

import math
import subprocess
from pathlib import Path

import pytest

from recall.asr import AsrSegment, build_slice_argv, decode_pcm_f32


def test_slice_argv_extracts_window() -> None:
    argv = build_slice_argv(Path("/a/clip.wav"), Path("/b/turn.wav"), 1.5, 4.25)
    assert argv[0] == "ffmpeg"
    assert argv[argv.index("-ss") + 1] == "1.500"
    assert argv[argv.index("-to") + 1] == "4.250"
    assert argv[-1] == "/b/turn.wav"


def test_decode_pcm_f32_returns_normalised_mono_waveform(tmp_path: Path) -> None:
    flac = tmp_path / "tone.flac"
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
            "1.0",
            "-ac",
            "1",
            "-c:a",
            "flac",
            str(flac),
        ],
        check=True,
    )

    wave = decode_pcm_f32(flac, sample_rate=16000)

    assert wave.ndim == 1
    assert wave.dtype.name == "float32"
    assert abs(len(wave) - 16000) <= 160  # within ~10ms of one second
    assert float(wave.max()) <= 1.0
    assert float(wave.min()) >= -1.0
    assert float(abs(wave).max()) > 0.1  # a tone, not silence


def test_confidence_from_avg_logprob() -> None:
    seg = AsrSegment(
        start=0.0, end=1.0, text="hi", avg_logprob=-0.1, no_speech_prob=0.0
    )
    assert math.isclose(seg.confidence, math.exp(-0.1))
    # clamped to [0, 1]
    confident = AsrSegment(start=0, end=1, text="x", avg_logprob=0.5, no_speech_prob=0)
    assert confident.confidence == 1.0


def test_the_slice_refuses_stdin() -> None:
    """ffmpeg reads stdin for its interactive controls, so run from a terminal
    it swallows keystrokes. The flag is in the argv, so this test holds it."""
    argv = build_slice_argv(Path("/a/clip.wav"), Path("/b/turn.wav"), 1.5, 4.25)
    assert "-nostdin" in argv
    # Only honoured as an input option.
    assert argv.index("-nostdin") < argv.index("-i")


def test_the_temperature_fallback_draws_fresh_noise_each_time() -> None:
    # As shipped, mlx-whisper's compiled sampler froze one random key.
    mx = pytest.importorskip("mlx.core")
    decoding = pytest.importorskip("mlx_whisper.decoding")
    from recall.asr import unfreeze_sampling  # noqa: PLC0415 - needs mlx

    unfreeze_sampling()
    unfreeze_sampling()  # idempotent
    flat = mx.zeros((1, 1000))
    draws = {decoding.categorical(flat, 1.0).item() for _ in range(8)}
    assert len(draws) > 1, draws
