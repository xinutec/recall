"""The `voices` shim's argument handling and wire form (stage E4).

pyannote is injected: what needs testing here is the contract with the runner,
which is what makes refinement movable off Python — not the model, which has no
business running in a unit test.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from recall.diarize import SpeakerTurn
from recall.shim import JsonValue
from recall.shim_voices import Diarize, Embed, handle


def as_dict(value: JsonValue) -> dict[str, JsonValue]:
    assert isinstance(value, dict), value
    return value


def as_list(value: JsonValue) -> list[JsonValue]:
    assert isinstance(value, list), value
    return value


def clip_at(tmp_path: Path, name: str = "usb-20260906T090000.flac") -> Path:
    clip = tmp_path / name
    clip.write_bytes(b"x")
    return clip


def diarizer(captured: dict[str, object]) -> Diarize:
    def diarize(
        audio: Path,
        /,
        *,
        model: str,
        clustering_threshold: float | None,
        min_cluster_size: int | None,
    ) -> list[SpeakerTurn]:
        captured.update(
            audio=audio,
            model=model,
            clustering_threshold=clustering_threshold,
            min_cluster_size=min_cluster_size,
        )
        return [
            SpeakerTurn(speaker="SPEAKER_01", start=1.5, end=3.0),
            SpeakerTurn(speaker="SPEAKER_00", start=0.0, end=1.5),
        ]

    return diarize


def embedder(captured: dict[str, object]) -> Embed:
    def embed(audio: Path, /, *, model: str) -> list[float]:
        captured.update(audio=audio, model=model)
        return [0.5, -0.25, 0.125]

    return embed


def test_a_diarize_request_reaches_the_model_with_its_arguments(
    tmp_path: Path,
) -> None:
    clip = clip_at(tmp_path)
    seen: dict[str, object] = {}
    out = handle(
        "diarize",
        {
            "audio": str(clip),
            "model": "pyannote/speaker-diarization-3.1",
            "clustering_threshold": 0.7,
            "min_cluster_size": 3,
        },
        diarize=diarizer(seen),
    )
    assert seen["audio"] == clip
    assert seen["model"] == "pyannote/speaker-diarization-3.1"
    assert seen["clustering_threshold"] == 0.7
    assert seen["min_cluster_size"] == 3
    first = as_dict(as_list(as_dict(out)["turns"])[0])
    assert first["speaker"] == "SPEAKER_01"
    assert first["start"] == 1.5


def test_diarize_defaults_leave_the_pipeline_exactly_as_shipped(
    tmp_path: Path,
) -> None:
    # Both None is the production path, and it must stay the untuned one: a
    # default that quietly tuned would change what the whole archive diarizes.
    seen: dict[str, object] = {}
    handle("diarize", {"audio": str(clip_at(tmp_path))}, diarize=diarizer(seen))
    assert seen["model"]  # DEFAULT_DIARIZER
    assert seen["clustering_threshold"] is None
    assert seen["min_cluster_size"] is None


def test_an_embed_request_returns_the_vector(tmp_path: Path) -> None:
    seen: dict[str, object] = {}
    out = handle("embed", {"audio": str(clip_at(tmp_path))}, embed=embedder(seen))
    assert seen["model"]  # DEFAULT_EMBEDDER
    assert as_list(as_dict(out)["vector"]) == [0.5, -0.25, 0.125]


def test_a_missing_clip_is_refused_clearly_on_both_ops(tmp_path: Path) -> None:
    for op in ("diarize", "embed"):
        with pytest.raises(FileNotFoundError):
            handle(op, {"audio": str(tmp_path / "nope.flac")})


def test_a_request_without_audio_is_refused(tmp_path: Path) -> None:
    for op in ("diarize", "embed"):
        with pytest.raises(ValueError, match="audio path"):
            handle(op, {})


def test_an_unknown_op_is_refused() -> None:
    with pytest.raises(ValueError, match="unknown op"):
        handle("identify", {"audio": "/x"})
