"""The `asr` shim (stage E2): mlx-whisper behind the stdio protocol.

Run as `python -m recall.shim_asr`. It holds the weights for the life of the
process and answers one `transcribe` at a time, which is what makes it worth
being a process at all — loading a Whisper model costs seconds, and the old
per-clip cost is a bill this architecture stops paying.

⚠ **It reads no database and no config.** The doc's rule is that a shim does no
I/O beyond its stdio and the audio path it is handed, and vocabulary biasing is
the interesting case: `initial_prompt` is what teaches Whisper to spell the
household's names, and it comes FROM the caller. Letting the shim fetch it would
put a DB handle, a schema and a failure mode inside the process whose only job
is to run a model — and would couple the Mac worker back to state it is supposed
to have given up (docs/architecture.md, principle 3).
"""

from __future__ import annotations

from pathlib import Path
from typing import Protocol

from recall import shim
from recall.asr import DEFAULT_MODEL, AsrResult, Excerpt, mlx_transcribe
from recall.shim import JsonDict, JsonValue


class Transcribe(Protocol):
    """What the shim needs of a transcriber — the real one is `mlx_transcribe`."""

    def __call__(
        self,
        audio: Path | Excerpt,
        /,
        *,
        model: str,
        language: str | None,
        words: bool,
        initial_prompt: str | None,
    ) -> AsrResult: ...


NAME = "asr"


def result_to_json(result: AsrResult) -> JsonDict:
    """The wire form of an `AsrResult`.

    Word timings ride along only when they were asked for, so a caller that does
    not need them does not pay to serialise them.
    """
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
    """Answer one request. `transcribe` is injected so the protocol and the
    argument handling are testable without Apple Silicon or 1.5 GB of weights."""
    if op != "transcribe":
        raise ValueError(f"unknown op: {op}")
    audio = args.get("audio")
    if not isinstance(audio, str) or not audio:
        raise ValueError("transcribe needs an audio path")
    path = Path(audio)
    if not path.is_file():
        # A clear refusal beats whatever the model would say about a missing
        # file, and the runner can ack and move on.
        raise FileNotFoundError(audio)
    # A window (seconds from the clip's start) is transcribed alone: the
    # silence around speech can send Whisper into a loop (#1764).
    start, end = args.get("start"), args.get("end")
    audio_in: Path | Excerpt = path
    if start is not None or end is not None:
        if not isinstance(start, int | float) or not isinstance(end, int | float):
            raise ValueError("a window needs both start and end, in seconds")
        audio_in = Excerpt(path, float(start), float(end))
    result = transcribe(
        audio_in,
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
