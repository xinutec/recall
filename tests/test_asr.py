"""ASR result mapping and working-copy command construction (pure, no model)."""

from __future__ import annotations

import math
import subprocess
from datetime import UTC, datetime, timedelta
from pathlib import Path

from recall.asr import (
    AsrResult,
    AsrSegment,
    build_slice_argv,
    build_working_copy_argv,
    decode_pcm_f32,
    result_to_drafts,
)

BASE = datetime(2026, 6, 13, 12, 0, 0, tzinfo=UTC)


def test_working_copy_is_mono_16k_normalised() -> None:
    argv = build_working_copy_argv(Path("/a/seg.flac"), Path("/b/seg.wav"))
    assert argv[0] == "ffmpeg"
    assert argv[argv.index("-ar") + 1] == "16000"
    assert argv[argv.index("-ac") + 1] == "1"
    # loudness-normalised for the ASR copy (raw archive is untouched)
    assert "loudnorm" in argv[argv.index("-af") + 1]
    assert argv[-1] == "/b/seg.wav"


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

    # 1 second at 16 kHz, 1-D float32, in [-1, 1] — what a feature extractor wants.
    assert wave.ndim == 1
    assert wave.dtype.name == "float32"
    assert abs(len(wave) - 16000) <= 160  # within ~10ms of one second
    assert float(wave.max()) <= 1.0
    assert float(wave.min()) >= -1.0
    assert float(abs(wave).max()) > 0.1  # a real tone, not silence


def test_confidence_from_avg_logprob() -> None:
    seg = AsrSegment(
        start=0.0, end=1.0, text="hi", avg_logprob=-0.1, no_speech_prob=0.0
    )
    assert math.isclose(seg.confidence, math.exp(-0.1))
    # clamped to [0, 1]
    confident = AsrSegment(start=0, end=1, text="x", avg_logprob=0.5, no_speech_prob=0)
    assert confident.confidence == 1.0


def test_result_to_drafts_offsets_to_absolute_time() -> None:
    result = AsrResult(
        language="en",
        language_confidence=0.97,
        segments=(
            AsrSegment(
                start=0.0,
                end=2.0,
                text=" Hello ",
                avg_logprob=-0.2,
                no_speech_prob=0.0,
            ),
            AsrSegment(
                start=2.0,
                end=4.0,
                text="world",
                avg_logprob=-0.5,
                no_speech_prob=0.0,
            ),
        ),
    )
    drafts = result_to_drafts(result, segment_start=BASE, model_name="whisper-x")

    assert len(drafts) == 2
    assert drafts[0].start == BASE
    assert drafts[0].end == BASE + timedelta(seconds=2)
    assert drafts[0].text == "Hello"  # stripped
    assert drafts[0].language == "en"
    assert drafts[0].language_confidence == 0.97
    assert drafts[0].asr_model == "whisper-x"
    assert drafts[1].start == BASE + timedelta(seconds=2)


def test_result_to_drafts_skips_empty_text() -> None:
    result = AsrResult(
        language="en",
        language_confidence=None,
        segments=(
            AsrSegment(
                start=0.0,
                end=1.0,
                text="   ",
                avg_logprob=-0.1,
                no_speech_prob=0.9,
            ),
            AsrSegment(
                start=1.0,
                end=2.0,
                text="real",
                avg_logprob=-0.1,
                no_speech_prob=0.0,
            ),
        ),
    )
    drafts = result_to_drafts(result, segment_start=BASE, model_name="m")
    assert [d.text for d in drafts] == ["real"]
