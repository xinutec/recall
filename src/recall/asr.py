"""Speech recognition: the result types, audio slicing and decoding, and the
mlx-whisper call, which imports lazily so the rest is testable without it.
"""

from __future__ import annotations

import math
import subprocess
from collections.abc import Iterator
from contextlib import contextmanager
from dataclasses import dataclass
from functools import partial
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    import numpy as np

DEFAULT_MODEL = "mlx-community/whisper-large-v3-turbo"


@dataclass(frozen=True)
class Word:
    """One word, timed from the start of the clip."""

    start: float
    end: float
    text: str
    probability: float


@dataclass(frozen=True)
class AsrSegment:
    """One transcribed span, timed from the start of the clip."""

    start: float
    end: float
    text: str
    avg_logprob: float
    no_speech_prob: float
    words: tuple[Word, ...] = ()

    @property
    def confidence(self) -> float:
        """A [0, 1] confidence proxy from the mean token log-probability."""
        return min(1.0, max(0.0, math.exp(self.avg_logprob)))


@dataclass(frozen=True)
class AsrResult:
    """The transcription of one clip."""

    language: str
    language_confidence: float | None
    segments: tuple[AsrSegment, ...]


def build_slice_argv(src: Path, dst: Path, start: float, end: float) -> list[str]:
    """ffmpeg argv to extract seconds `start` to `end` of `src`. `-nostdin`
    before `-i`, or ffmpeg reads the terminal's keystrokes."""
    return [
        "ffmpeg",
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-i",
        str(src),
        "-ss",
        f"{start:.3f}",
        "-to",
        f"{end:.3f}",
        str(dst),
    ]


def slice_clip(src: Path, dst: Path, start: float, end: float) -> None:
    """Extract seconds `start` to `end` of `src` into `dst`."""
    dst.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(build_slice_argv(src, dst, start, end), check=True)


@contextmanager
def scratch_wav(path: Path) -> Iterator[Path]:
    """Yield `path` for a scratch clip, deleting it on exit."""
    try:
        yield path
    finally:
        path.unlink(missing_ok=True)


def decode_pcm_f32(audio: Path, *, sample_rate: int = 16000) -> np.ndarray:
    """Decode `audio` to mono float32 samples in [-1, 1] at `sample_rate`, via
    ffmpeg: torchcodec cannot load its shared libraries on this torch stack.
    """
    import numpy as np  # noqa: PLC0415 - keep numpy out of the module import surface

    pcm = subprocess.run(
        [
            "ffmpeg",
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            str(audio),
            "-ac",
            "1",
            "-ar",
            str(sample_rate),
            "-f",
            "s16le",
            "-",
        ],
        capture_output=True,
        check=True,
    ).stdout
    return np.frombuffer(pcm, dtype=np.int16).astype(np.float32) / 32768.0


def _extract_words(segment: dict[str, object]) -> tuple[Word, ...]:
    raw = segment.get("words")
    if not isinstance(raw, list):
        return ()
    return tuple(
        Word(
            start=float(w["start"]),
            end=float(w["end"]),
            text=str(w["word"]),
            probability=float(w.get("probability", 1.0)),
        )
        for w in raw
    )


_UNFROZEN: set[object] = set()
"""The sampler `unfreeze_sampling` installed: a compiled function takes no marker."""


def unfreeze_sampling() -> None:
    """Make mlx-whisper's temperature fallback draw fresh noise per token.
    Idempotent.

    Its `categorical` is `@mx.compile`d without the random state as an input,
    which freezes one key, so the fallback meant to escape a repetition loop
    repeats the same noise and makes loops. On four stuck clips, three runs
    each: 7 of 15 looped as shipped, 0 of 15 with the state passed (#1764).
    """
    import mlx.core as mx  # noqa: PLC0415 - lazy, as mlx_whisper
    from mlx_whisper import decoding  # noqa: PLC0415 - lazy, as mlx_whisper

    if decoding.categorical in _UNFROZEN:
        return

    @partial(mx.compile, inputs=mx.random.state, outputs=mx.random.state)
    def categorical(logits: mx.array, temp: float) -> mx.array:
        return mx.random.categorical(logits / temp)

    _UNFROZEN.add(categorical)
    decoding.categorical = categorical


def mlx_transcribe(
    audio: Path,
    *,
    model: str = DEFAULT_MODEL,
    language: str | None = None,
    words: bool = False,
    initial_prompt: str | None = None,
) -> AsrResult:
    """Transcribe `audio` with mlx-whisper.

    `language` ("en", "nl") forces a language; None detects it. `words` adds
    word timings. `initial_prompt` is the vocabulary
    (`recalld::labels::initial_prompt`), which gets names spelled right.
    """
    import mlx_whisper  # noqa: PLC0415 - lazy: mlx-whisper is an optional heavy dep

    unfreeze_sampling()

    # Against hallucination: no conditioning on the previous window, so a loop
    # cannot feed itself, and a window that trips the compression-ratio
    # (repetition) or logprob (gibberish) threshold is decoded again warmer.
    # Both need context, so whole segments are transcribed, not slices.
    raw = mlx_whisper.transcribe(
        str(audio),
        path_or_hf_repo=model,
        language=language,
        initial_prompt=initial_prompt,
        word_timestamps=words,
        condition_on_previous_text=False,
        temperature=(0.0, 0.2, 0.4, 0.6, 0.8, 1.0),
        compression_ratio_threshold=2.4,
        logprob_threshold=-1.0,
        no_speech_threshold=0.6,
    )
    segments = tuple(
        AsrSegment(
            start=float(s["start"]),
            end=float(s["end"]),
            text=str(s["text"]),
            avg_logprob=float(s["avg_logprob"]),
            no_speech_prob=float(s["no_speech_prob"]),
            words=_extract_words(s) if words else (),
        )
        for s in raw["segments"]
    )
    return AsrResult(
        language=str(raw["language"]), language_confidence=None, segments=segments
    )
