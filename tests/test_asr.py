"""ASR result mapping and working-copy command construction (pure, no model)."""

from __future__ import annotations

import math
import subprocess
from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest

from conftest import make_flac
from recall.asr import (
    AsrResult,
    AsrSegment,
    build_concat_argv,
    build_slice_argv,
    build_working_copy_argv,
    concat_working_copy,
    decode_pcm_f32,
    result_to_drafts,
)
from recall.probe import probe_media

BASE = datetime(2026, 6, 13, 12, 0, 0, tzinfo=UTC)


def test_working_copy_is_mono_16k_normalised() -> None:
    argv = build_working_copy_argv(Path("/a/seg.flac"), Path("/b/seg.wav"))
    assert argv[0] == "ffmpeg"
    assert argv[argv.index("-ar") + 1] == "16000"
    assert argv[argv.index("-ac") + 1] == "1"
    # loudness-normalised for the ASR copy (raw archive is untouched)
    assert "loudnorm" in argv[argv.index("-af") + 1]
    assert argv[-1] == "/b/seg.wav"


def test_concat_argv_joins_every_input_and_normalises_once() -> None:
    argv = build_concat_argv(
        [Path("/a/1.opus"), Path("/a/2.opus"), Path("/a/3.opus")],
        Path("/b/run.wav"),
        normalize=True,
    )
    assert argv[0] == "ffmpeg"
    assert argv.count("-i") == 3
    graph = argv[argv.index("-filter_complex") + 1]
    assert "[0:a][1:a][2:a]concat=n=3" in graph
    assert graph.endswith("[j];[j]loudnorm[out]")
    assert argv[argv.index("-ar") + 1] == "16000"
    assert argv[argv.index("-ac") + 1] == "1"
    assert argv[-1] == "/b/run.wav"


def test_concat_can_skip_normalising_so_a_join_changes_nothing_else() -> None:
    # The comparison-safe mode: sources are already normalised, so joining them must
    # not re-gain the audio — otherwise a windowed run differs from its baseline in
    # two ways at once and neither can be attributed.
    argv = build_concat_argv(
        [Path("/a/1.wav"), Path("/a/2.wav")], Path("/b/run.wav"), normalize=False
    )
    graph = argv[argv.index("-filter_complex") + 1]
    assert "loudnorm" not in graph
    assert graph.endswith("[j];[j]anull[out]")


def test_concat_needs_a_source(tmp_path: Path) -> None:
    with pytest.raises(ValueError, match="at least one source"):
        concat_working_copy([], tmp_path / "out.wav", normalize=True)


def test_concat_really_joins_audio(tmp_path: Path) -> None:
    # End-to-end through ffmpeg: three 1s tones must come back as ~3s of audio, so the
    # filter graph is right and not just plausible.
    parts = []
    for i in range(3):
        part = tmp_path / f"p{i}.flac"
        make_flac(part, seconds=1.0)
        parts.append(part)
    out = tmp_path / "joined.wav"
    concat_working_copy(parts, out, normalize=True)
    assert out.exists()
    assert 2.5 <= probe_media(out).duration.total_seconds() <= 3.5


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


def test_every_derived_copy_builder_refuses_stdin() -> None:
    """ffmpeg reads stdin for its interactive controls, so it eats the parent's.

    Invisible under launchd, where stdin is closed — which is why this went
    unnoticed. It shows up when a person runs `scripts/recall.sh transcribe`
    from a terminal and ffmpeg swallows the keystrokes, once per segment.

    The flag lives in the argv rather than `stdin=DEVNULL` at the call site so
    that it travels with the command and this test is what holds it there.
    """
    builders = [
        build_working_copy_argv(Path("/a/seg.flac"), Path("/b/seg.wav")),
        build_concat_argv(
            [Path("/a/1.opus"), Path("/a/2.opus")], Path("/b/run.wav"), normalize=True
        ),
        build_slice_argv(Path("/a/clip.wav"), Path("/b/turn.wav"), 1.5, 4.25),
    ]
    for argv in builders:
        assert "-nostdin" in argv, argv
        # Before the first -i: ffmpeg only honours it as an input option.
        assert argv.index("-nostdin") < argv.index("-i"), argv
