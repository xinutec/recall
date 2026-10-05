"""The `asr` shim: mlx-whisper behind the stdio protocol.

Run as `python -m recall.shim_asr`. It keeps the weights loaded, since loading
costs seconds, and answers one `transcribe` at a time.

It reads no database or config: even the vocabulary (`initial_prompt`) comes
from the caller (docs/architecture.md).
"""

from __future__ import annotations

from pathlib import Path
from typing import Protocol

from recall import shim
from recall.asr import DEFAULT_MODEL, AsrResult, mlx_transcribe
from recall.shim import JsonDict, JsonValue


class Transcribe(Protocol):
    """A transcriber; the real one is `mlx_transcribe`."""

    def __call__(
        self,
        audio: Path,
        /,
        *,
        model: str,
        language: str | None,
        words: bool,
        initial_prompt: str | None,
    ) -> AsrResult: ...


NAME = "asr"


def result_to_json(result: AsrResult) -> JsonDict:
    """The wire form of an `AsrResult`."""
    return {
        "language": result.language,
        "language_confidence": result.language_confidence,
        "segments": [
            {
                "start": s.start,
                "end": s.end,
                "text": s.text,
                "avg_logprob": s.avg_logprob,
                "no_speech_prob": s.no_speech_prob,
                "confidence": s.confidence,
                "words": [
                    {
                        "start": w.start,
                        "end": w.end,
                        "text": w.text,
                        "probability": w.probability,
                    }
                    for w in s.words
                ],
            }
            for s in result.segments
        ],
    }


def handle(
    op: str, args: JsonDict, *, transcribe: Transcribe = mlx_transcribe
) -> JsonValue:
    """Answer one request. `transcribe` is a parameter so tests need neither
    Apple Silicon nor the weights."""
    if op != "transcribe":
        raise ValueError(f"unknown op: {op}")
    audio = args.get("audio")
    if not isinstance(audio, str) or not audio:
        raise ValueError("transcribe needs an audio path")
    path = Path(audio)
    if not path.is_file():
        raise FileNotFoundError(audio)
    result = transcribe(
        path,
        model=str(args.get("model") or DEFAULT_MODEL),
        language=str(args["language"]) if args.get("language") else None,
        words=bool(args.get("words", False)),
        initial_prompt=(
            str(args["initial_prompt"]) if args.get("initial_prompt") else None
        ),
    )
    return result_to_json(result)


def main() -> None:
    shim.serve(handle, name=NAME)


if __name__ == "__main__":
    main()
