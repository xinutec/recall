"""The `voices` shim (stage E4): pyannote behind the stdio protocol.

Run as `python -m recall.shim_voices`. The second shim after `asr`, and the one
that lets the Rust runner own refinement: diarization says who spoke when, and
embedding turns a clip into the vector that names them. Both are pyannote, both
are Python only because the model is.

⚠ **It reads no database, and that is the whole point of it existing.** Today
`recall refine` holds a `Store`, the pyannote pipeline, the coverage guards and
the write transaction in one process — which is why refinement can only live on
the Mac. Splitting the model call out leaves the guards and the write to be
re-homed in Rust (they are the part that blanked 132 segments when it was got
wrong, so they move on their own, with their own tests). This shim is the half
that cannot move, and it is 100 lines.

Matching against enrolled voiceprints is deliberately NOT here. `identify` is
pure arithmetic over vectors — `speakerid.cosine_similarity` — and pure
arithmetic belongs on the side that owns the profiles, not inside the process
holding the weights.
"""

from __future__ import annotations

from pathlib import Path
from typing import Protocol

from recall import shim
from recall.diarize import DEFAULT_DIARIZER, SpeakerTurn, pyannote_diarize
from recall.shim import JsonDict, JsonValue
from recall.speakerid import pyannote_embed

DEFAULT_EMBEDDER = "pyannote/embedding"

NAME = "voices"


class Diarize(Protocol):
    """What the shim needs of a diarizer — the real one is `pyannote_diarize`."""

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
    """What the shim needs of an embedder — the real one is `pyannote_embed`."""

    def __call__(self, audio: Path, /, *, model: str) -> list[float]: ...


def _clip(args: JsonDict) -> Path:
    """The audio path from a request, or a refusal naming what was wrong.

    A missing file is refused here rather than left to the model: the runner can
    ack a job it cannot do and move on, where a library's own error arrives as
    whatever that library felt like saying.
    """
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


def handle(
    op: str,
    args: JsonDict,
    *,
    diarize: Diarize = pyannote_diarize,
    embed: Embed = pyannote_embed,
) -> JsonValue:
    """Answer one request. The collaborators are injected so the protocol and the
    argument handling are testable without a gated download or 2 GB of weights."""
    if op == "diarize":
        turns = diarize(
            _clip(args),
            model=str(args.get("model") or DEFAULT_DIARIZER),
            clustering_threshold=_optional_float(args, "clustering_threshold"),
            min_cluster_size=_optional_int(args, "min_cluster_size"),
        )
        return {
            "turns": [
                {"speaker": t.speaker, "start": t.start, "end": t.end} for t in turns
            ]
        }
    if op == "embed":
        model = str(args.get("model") or DEFAULT_EMBEDDER)
        # Re-built as JsonValue rather than passed through: `list[float]` is not a
        # `list[JsonValue]` to a type checker, and the wire type is stated on
        # purpose (recall.shim) so an unserialisable result is an error here.
        vector: list[JsonValue] = list(embed(_clip(args), model=model))
        return {"vector": vector}
    raise ValueError(f"unknown op: {op}")


def main() -> None:
    shim.serve(handle, name=NAME)


if __name__ == "__main__":
    main()
