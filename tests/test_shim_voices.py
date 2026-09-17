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


def test_an_embed_request_can_name_a_span_within_the_clip(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Enrolment embeds ONE labelled turn, not the whole clip a turn sits in.

    A minute of room audio holding four seconds of the person being enrolled
    would make a voiceprint mostly of everyone else — so the span is the request,
    and the clip is only where it is cut from."""
    sliced: list[tuple[float, float]] = []

    def fake_slice(src: Path, dst: Path, start: float, end: float) -> None:
        sliced.append((start, end))
        dst.write_bytes(b"clip")

    monkeypatch.setattr("recall.shim_voices.slice_clip", fake_slice)
    out = as_dict(
        handle(
            "embed",
            {"audio": str(clip_at(tmp_path)), "start": 4.0, "end": 9.5},
            embed=_constant_embedder([0.25]),
        )
    )
    assert sliced == [(4.0, 9.5)]
    assert as_list(out["vector"]) == [0.25]


def test_an_embed_request_without_a_span_reads_the_whole_clip(tmp_path: Path) -> None:
    """The span is OPTIONAL, and its absence must not silently become 0..0 — a
    caller that wants the clip is the original contract and still has it."""
    seen: dict[str, object] = {}
    out = handle("embed", {"audio": str(clip_at(tmp_path))}, embed=embedder(seen))
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


def _fixed_turns() -> list[SpeakerTurn]:
    """⚠ SPEAKER_00 holds a 0.4s backchannel AND a 6.0s explanation. A voiceprint
    built from the backchannel is worse than one built from the explanation, and
    diarization returns both — so which span is chosen is a decision, not a detail."""
    return [
        SpeakerTurn(speaker="SPEAKER_00", start=0.0, end=0.4),
        SpeakerTurn(speaker="SPEAKER_01", start=0.4, end=3.0),
        SpeakerTurn(speaker="SPEAKER_00", start=3.0, end=9.0),
    ]


def _fixed(turns: list[SpeakerTurn]) -> Diarize:
    def diarize(
        audio: Path,
        /,
        *,
        model: str,
        clustering_threshold: float | None,
        min_cluster_size: int | None,
    ) -> list[SpeakerTurn]:
        return turns

    return diarize


def _constant_embedder(vector: list[float]) -> Embed:
    def embed(audio: Path, /, *, model: str) -> list[float]:
        return vector

    return embed


def test_diarize_embeds_each_speaker_from_their_longest_span(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    sliced: list[tuple[float, float]] = []

    def fake_slice(src: Path, dst: Path, start: float, end: float) -> None:
        sliced.append((start, end))
        dst.write_bytes(b"clip")

    monkeypatch.setattr("recall.shim_voices.slice_clip", fake_slice)
    answer = as_dict(
        handle(
            "diarize",
            {"audio": str(clip_at(tmp_path)), "embed": True},
            diarize=_fixed(_fixed_turns()),
            embed=_constant_embedder([0.5, 0.5]),
        )
    )

    speakers = [as_dict(s) for s in as_list(answer["speakers"])]
    assert [s["speaker"] for s in speakers] == ["SPEAKER_00", "SPEAKER_01"]
    # SPEAKER_00's 6.0s span won over its 0.4s one.
    assert sliced == [(3.0, 9.0), (0.4, 3.0)]
    assert speakers[0]["seconds"] == 6.0


def test_diarize_without_embed_sends_no_vectors(tmp_path: Path) -> None:
    """Opt-in: the embedding costs a model load and a slice per speaker, and a
    caller that only wants spans must not pay for it."""
    answer = as_dict(
        handle(
            "diarize",
            {"audio": str(clip_at(tmp_path))},
            diarize=_fixed(_fixed_turns()),
            embed=_constant_embedder([1.0]),
        )
    )
    assert "speakers" not in answer


def test_a_speaker_whose_clip_will_not_slice_is_skipped_not_faked(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A corrupt frame costs that speaker a name, never the whole reply."""

    def bad_slice(src: Path, dst: Path, start: float, end: float) -> None:
        if start == 3.0:
            msg = "ffmpeg refused"
            raise RuntimeError(msg)
        dst.write_bytes(b"clip")

    monkeypatch.setattr("recall.shim_voices.slice_clip", bad_slice)
    answer = as_dict(
        handle(
            "diarize",
            {"audio": str(clip_at(tmp_path)), "embed": True},
            diarize=_fixed(_fixed_turns()),
            embed=_constant_embedder([0.1]),
        )
    )

    speakers = [as_dict(s) for s in as_list(answer["speakers"])]
    assert [s["speaker"] for s in speakers] == ["SPEAKER_01"]
