"""The runner-shim contract, from the Python side (#1830).

`tests/fixtures/shim/` holds one example of every message between the runner
and the shims. Here the real handlers, with their models faked, must produce
exactly the reply files and accept the request files; `audiocore/tests/shim.rs`
holds the Rust types to the same files. A shape changed on one side alone fails
one of the two.

`RECALL_WRITE_SHIM_FIXTURES=1` writes the reply files from the handlers instead
of comparing, for a deliberate change of shape.
"""

from __future__ import annotations

import json
import os
from pathlib import Path

import pytest

from recall.asr import AsrResult, AsrSegment, Word
from recall.diarize import SpeakerTurn
from recall.shim import JsonDict, JsonValue
from recall.shim_asr import handle as asr
from recall.shim_voices import handle as voices

FIXTURES = Path(__file__).parent / "fixtures" / "shim"
WRITE = os.environ.get("RECALL_WRITE_SHIM_FIXTURES") == "1"


def fixture(name: str) -> JsonDict:
    value: JsonValue = json.loads((FIXTURES / name).read_text())
    assert isinstance(value, dict), name
    return value


def matches(name: str, produced: JsonValue) -> None:
    """The handler's reply is the committed example, byte for byte as JSON."""
    path = FIXTURES / name
    if WRITE:
        path.write_text(json.dumps(produced, indent=2, ensure_ascii=False) + "\n")
    assert produced == fixture(name)


@pytest.fixture
def clip(tmp_path: Path) -> Path:
    path = tmp_path / "usb-20260906T090000.flac"
    path.write_bytes(b"x")
    return path


def with_clip(request: JsonDict, clip: Path) -> JsonDict:
    """A request file names a clip that exists only in the runner's scratch dir."""
    return {**request, "audio": str(clip)}


def test_the_transcribe_request_is_read_and_the_reply_is_the_example(
    clip: Path,
) -> None:
    seen: dict[str, object] = {}

    def transcribe(
        audio: Path,
        /,
        *,
        model: str,
        language: str | None,
        words: bool,
        initial_prompt: str | None,
    ) -> AsrResult:
        seen.update(words=words, initial_prompt=initial_prompt, language=language)
        return AsrResult(
            language="en",
            language_confidence=None,
            segments=(
                AsrSegment(
                    start=0.0,
                    end=1.5,
                    text=" hello there",
                    avg_logprob=-0.25,
                    no_speech_prob=0.01,
                    words=(
                        Word(start=0.0, end=0.5, text=" hello", probability=0.875),
                        Word(start=0.5, end=1.5, text=" there", probability=0.75),
                    ),
                ),
            ),
        )

    request = fixture("transcribe-request.json")
    reply = asr("transcribe", with_clip(request, clip), transcribe=transcribe)
    assert seen == {"words": True, "initial_prompt": "Alex, Sam", "language": None}
    matches("transcribe-reply.json", reply)


def test_the_detect_request_is_read_and_the_reply_is_the_example(clip: Path) -> None:
    seen: dict[str, object] = {}

    def detect(audio: Path, /, *, model: str) -> tuple[str, float]:
        seen.update(audio=audio)
        return "nl", 0.875

    request = fixture("detect-request.json")
    reply = asr("detect-language", with_clip(request, clip), detect=detect)
    assert seen == {"audio": clip}
    matches("detect-reply.json", reply)


def test_the_diarize_request_is_read_and_the_reply_is_the_example(
    clip: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    def fake_slice(src: Path, dst: Path, start: float, end: float) -> None:
        dst.write_bytes(b"clip")

    monkeypatch.setattr("recall.shim_voices.slice_clip", fake_slice)

    def diarize(
        audio: Path,
        /,
        *,
        model: str,
        clustering_threshold: float | None,
        min_cluster_size: int | None,
    ) -> list[SpeakerTurn]:
        return [
            SpeakerTurn(speaker="SPEAKER_00", start=0.0, end=2.5),
            SpeakerTurn(speaker="SPEAKER_01", start=2.5, end=4.0),
        ]

    def embed(audio: Path, /, *, model: str) -> list[float]:
        return [0.5, -0.25]

    request = fixture("diarize-request.json")
    reply = voices("diarize", with_clip(request, clip), diarize=diarize, embed=embed)
    matches("diarize-reply.json", reply)


def test_the_embed_request_is_read_and_the_reply_is_the_example(
    clip: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    sliced: list[tuple[float, float]] = []

    def fake_slice(src: Path, dst: Path, start: float, end: float) -> None:
        sliced.append((start, end))
        dst.write_bytes(b"clip")

    monkeypatch.setattr("recall.shim_voices.slice_clip", fake_slice)

    def embed(audio: Path, /, *, model: str) -> list[float]:
        return [0.125, 0.25]

    request = fixture("embed-request.json")
    reply = voices("embed", with_clip(request, clip), embed=embed)
    assert sliced == [(1.5, 4.0)]
    matches("embed-reply.json", reply)
