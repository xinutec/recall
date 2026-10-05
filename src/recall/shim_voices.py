"""The `voices` shim: pyannote diarization and embedding behind the stdio
protocol. Run as `python -m recall.shim_voices`.

Only the model calls are here. What is done with the turns is in
`recalld::diarized`, and matching vectors to enrolled voices in
`recalld::identify`.
"""

from __future__ import annotations

from pathlib import Path
from typing import Protocol

from recall import shim
from recall.asr import scratch_wav, slice_clip
from recall.diarize import DEFAULT_DIARIZER, SpeakerTurn, pyannote_diarize
from recall.shim import JsonDict, JsonValue
from recall.speakerid import pyannote_embed

DEFAULT_EMBEDDER = "pyannote/embedding"

NAME = "voices"


class Diarize(Protocol):
    """A diarizer; the real one is `pyannote_diarize`."""

    def __call__(
        self,
        audio: Path,
        /,
        *,
        model: str,
        clustering_threshold: float | None,
        min_cluster_size: int | None,
    ) -> list[SpeakerTurn]: ...


class Embed(Protocol):
    """An embedder; the real one is `pyannote_embed`."""

    def __call__(self, audio: Path, /, *, model: str) -> list[float]: ...


def _clip(args: JsonDict) -> Path:
    """The audio path from a request; a missing file is refused here, with a
    clearer error than the library's."""
    audio = args.get("audio")
    if not isinstance(audio, str) or not audio:
        raise ValueError("needs an audio path")
    path = Path(audio)
    if not path.is_file():
        raise FileNotFoundError(audio)
    return path


def _optional_float(args: JsonDict, key: str) -> float | None:
    value = args.get(key)
    return float(value) if isinstance(value, (int, float)) else None


def _optional_int(args: JsonDict, key: str) -> int | None:
    value = args.get(key)
    return int(value) if isinstance(value, int) else None


def _embed_speakers(
    audio: Path, turns: list[SpeakerTurn], embed: Embed, model: str
) -> list[JsonValue]:
    """One vector per speaker in the clip, from their longest span. Per speaker,
    not per line: lines are aligned on the server, after this job.

    A span ffmpeg cannot slice (a corrupt frame) is skipped; that speaker gets
    no name guess.
    """
    longest: dict[str, SpeakerTurn] = {}
    for turn in turns:
        best = longest.get(turn.speaker)
        if best is None or (turn.end - turn.start) > (best.end - best.start):
            longest[turn.speaker] = turn
    out: list[JsonValue] = []
    for speaker, turn in sorted(longest.items()):
        try:
            with scratch_wav(audio.parent / f"{audio.stem}-{speaker}.wav") as clip:
                slice_clip(audio, clip, turn.start, turn.end)
                vector: list[JsonValue] = list(embed(clip, model=model))
        except Exception:  # costs this speaker, not the reply
            continue
        out.append(
            {
                "speaker": speaker,
                "seconds": turn.end - turn.start,
                "vector": vector,
            }
        )
    return out


def handle(
    op: str,
    args: JsonDict,
    *,
    diarize: Diarize = pyannote_diarize,
    embed: Embed = pyannote_embed,
) -> JsonValue:
    """Answer one request. `diarize` and `embed` are parameters so tests need
    no weights."""
    if op == "diarize":
        audio = _clip(args)
        turns = diarize(
            audio,
            model=str(args.get("model") or DEFAULT_DIARIZER),
            clustering_threshold=_optional_float(args, "clustering_threshold"),
            min_cluster_size=_optional_int(args, "min_cluster_size"),
        )
        answer: JsonDict = {
            "turns": [
                {"speaker": t.speaker, "start": t.start, "end": t.end} for t in turns
            ]
        }
        if args.get("embed"):
            answer["speakers"] = _embed_speakers(
                audio, turns, embed, str(args.get("embed_model") or DEFAULT_EMBEDDER)
            )
        return answer
    if op == "embed":
        model = str(args.get("model") or DEFAULT_EMBEDDER)
        audio = _clip(args)
        start = _optional_float(args, "start")
        end = _optional_float(args, "end")
        # A span, when given: enrolment names one turn, and the rest of the
        # clip holds other voices. Half a span is a caller bug.
        if (start is None) != (end is None):
            raise ValueError("embed: start and end are given together or not at all")
        # Copied into a `list[JsonValue]`, which `list[float]` is not to mypy.
        if start is None or end is None:
            vector: list[JsonValue] = list(embed(audio, model=model))
            return {"vector": vector}
        with scratch_wav(audio.parent / f"{audio.stem}-span.wav") as span:
            slice_clip(audio, span, start, end)
            vector = list(embed(span, model=model))
        return {"vector": vector}
    raise ValueError(f"unknown op: {op}")


def main() -> None:
    shim.serve(handle, name=NAME)


if __name__ == "__main__":
    main()
