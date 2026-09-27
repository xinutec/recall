"""The `asr` shim's argument handling and wire form (stage E2).

The model itself is injected: what needs testing here is the contract with the
runner, not mlx-whisper, which has no business running in a unit test.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from recall.asr import AsrResult, AsrSegment, Word
from recall.shim import JsonValue
from recall.shim_asr import Transcribe, handle, result_to_json


def as_dict(value: JsonValue) -> dict[str, JsonValue]:
    """Narrow one wire value. The protocol's type is a UNION on purpose — a
    handler may return any JSON — so reading a known shape out of it is an
    assertion about the contract, and worth making explicitly in a test."""
    assert isinstance(value, dict), value
    return value


def as_list(value: JsonValue) -> list[JsonValue]:
    assert isinstance(value, list), value
    return value


def segment(out: JsonValue, index: int = 0) -> dict[str, JsonValue]:
    return as_dict(as_list(as_dict(out)["segments"])[index])


def fake_result() -> AsrResult:
    return AsrResult(
        language="nl",
        language_confidence=0.9,
        segments=(
            AsrSegment(
                start=0.0,
                end=1.5,
                text="hallo daar",
                avg_logprob=-0.2,
                no_speech_prob=0.01,
                words=(Word(start=0.0, end=0.5, text="hallo", probability=0.99),),
            ),
        ),
    )


def recorder(captured: dict[str, object]) -> Transcribe:
    def transcribe(
        audio: Path,
        /,
        *,
        model: str,
        language: str | None,
        words: bool,
        initial_prompt: str | None,
    ) -> AsrResult:
        captured.update(
            audio=audio,
            model=model,
            language=language,
            words=words,
            initial_prompt=initial_prompt,
        )
        return fake_result()

    return transcribe


def test_a_transcribe_request_reaches_the_model_with_its_arguments(
    tmp_path: Path,
) -> None:
    clip = tmp_path / "usb-20260906T090000.flac"
    clip.write_bytes(b"x")
    seen: dict[str, object] = {}
    out = handle(
        "transcribe",
        {
            "audio": str(clip),
            "model": "mlx-community/whisper-small",
            "language": "nl",
            "words": True,
            "initial_prompt": "Pippijn, Kat",
        },
        transcribe=recorder(seen),
    )
    assert seen["audio"] == clip
    assert seen["model"] == "mlx-community/whisper-small"
    assert seen["language"] == "nl"
    assert seen["words"] is True
    # Vocabulary biasing is CARRIED, not fetched: the shim reads no database.
    assert seen["initial_prompt"] == "Pippijn, Kat"
    assert as_dict(out)["language"] == "nl"
    assert segment(out)["text"] == "hallo daar"
    assert as_dict(as_list(segment(out)["words"])[0])["text"] == "hallo"


def test_defaults_are_applied_when_the_caller_omits_them(tmp_path: Path) -> None:
    clip = tmp_path / "a.flac"
    clip.write_bytes(b"x")
    seen: dict[str, object] = {}
    handle("transcribe", {"audio": str(clip)}, transcribe=recorder(seen))
    assert seen["model"]  # DEFAULT_MODEL
    assert seen["language"] is None  # auto-detect
    assert seen["words"] is False
    assert seen["initial_prompt"] is None


def test_a_missing_clip_is_refused_clearly(tmp_path: Path) -> None:
    # Better than whatever the model would say about a missing file, and the
    # runner can ack and move on.
    with pytest.raises(FileNotFoundError):
        handle("transcribe", {"audio": str(tmp_path / "nope.flac")})


def test_a_request_without_audio_is_refused(tmp_path: Path) -> None:
    with pytest.raises(ValueError, match="audio path"):
        handle("transcribe", {})


def test_an_unknown_op_is_refused(tmp_path: Path) -> None:
    with pytest.raises(ValueError, match="unknown op"):
        handle("summarise", {"audio": "/x"})


def test_word_timings_are_absent_from_the_wire_when_not_requested() -> None:
    bare = AsrResult(
        language="en",
        language_confidence=None,
        segments=(
            AsrSegment(
                start=0.0, end=1.0, text="hi", avg_logprob=-0.1, no_speech_prob=0.0
            ),
        ),
    )
    assert segment(result_to_json(bare))["words"] == []


def test_confidence_travels_so_the_runner_need_not_re_derive_it() -> None:
    confidence = segment(result_to_json(fake_result()))["confidence"]
    assert isinstance(confidence, float)
    assert 0.0 <= confidence <= 1.0
