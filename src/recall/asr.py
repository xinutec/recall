"""Speech recognition: working-copy derivation, result types, and mapping.

The actual model call (`mlx_transcribe`) is isolated and lazily imports
mlx-whisper, so the pure logic here — building the normalised working copy and
mapping a result to absolute-time transcript drafts — is testable without any
model. Anything that wants to transcribe takes a `Transcriber`.
"""

from __future__ import annotations

import math
import subprocess
from collections.abc import Iterator, Sequence
from contextlib import contextmanager
from dataclasses import dataclass
from datetime import datetime, timedelta
from pathlib import Path
from statistics import mean
from typing import TYPE_CHECKING, Protocol

if TYPE_CHECKING:
    import numpy as np

DEFAULT_MODEL = "mlx-community/whisper-large-v3-turbo"


@dataclass(frozen=True)
class Word:
    """One word with its clip-relative timing (Whisper word_timestamps). The
    timings are what let us assign words to diarized speakers."""

    start: float
    end: float
    text: str
    probability: float


@dataclass(frozen=True)
class AsrSegment:
    """One transcribed span, timed from the start of the working-copy clip."""

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

    @property
    def words(self) -> tuple[Word, ...]:
        """Every word across all segments, in order (empty unless transcribed with
        word_timestamps)."""
        return tuple(word for segment in self.segments for word in segment.words)


@dataclass(frozen=True)
class TranscriptDraft:
    """A transcript segment in absolute wall-clock time, ready to store."""

    start: datetime
    end: datetime
    text: str
    language: str
    language_confidence: float | None
    asr_confidence: float
    asr_model: str


class Transcriber(Protocol):
    """Anything that turns an audio file into an `AsrResult`."""

    def __call__(self, audio: Path, /) -> AsrResult: ...


def build_working_copy_argv(
    src: Path, dst: Path, *, sample_rate: int = 16000
) -> list[str]:
    """ffmpeg argv to derive the ASR-facing working copy.

    Mono, 16 kHz, loudness-normalised — what Whisper wants. This is a *derived*
    copy; the raw archive segment is never modified (DESIGN req #1).
    """
    return [
        "ffmpeg",
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-i",
        str(src),
        "-ac",
        "1",
        "-ar",
        str(sample_rate),
        "-af",
        "loudnorm",
        "-f",
        "wav",
        str(dst),
    ]


def make_working_copy(src: Path, dst: Path, *, sample_rate: int = 16000) -> None:
    """Produce the normalised working copy at `dst`."""
    dst.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        build_working_copy_argv(src, dst, sample_rate=sample_rate), check=True
    )


def build_concat_argv(
    sources: Sequence[Path], dst: Path, *, sample_rate: int = 16000, normalize: bool
) -> list[str]:
    """ffmpeg argv to join `sources` end-to-end into one working copy.

    Mono and 16 kHz like `build_working_copy_argv`, over several inputs — for treating a
    run of consecutive capture segments as the single recording it acoustically is.

    `normalize` decides where loudness normalisation happens, and it is not a detail.
    True applies one `loudnorm` across the join, which reads well but makes the joined
    audio differ from the same segments normalised singly — so a comparison against a
    per-segment baseline is measuring two changes at once. False expects the caller to
    have normalised each source already and only joins them, which isolates the join.
    Prefer False whenever the join is being compared against unjoined segments.

    Caller's job to pass only temporally adjacent sources, in order — nothing here
    checks it, and joining across a recording gap would invent adjacency, and with it a
    speaker change that never happened. `attribution.context_window` is what enforces
    adjacency for the eval.
    """
    argv = ["ffmpeg", "-nostdin", "-hide_banner", "-loglevel", "error", "-y"]
    for src in sources:
        argv += ["-i", str(src)]
    streams = "".join(f"[{i}:a]" for i in range(len(sources)))
    graph = f"{streams}concat=n={len(sources)}:v=0:a=1[j]"
    graph += ";[j]loudnorm[out]" if normalize else ";[j]anull[out]"
    argv += [
        "-filter_complex",
        graph,
        "-map",
        "[out]",
        "-ac",
        "1",
        "-ar",
        str(sample_rate),
        "-f",
        "wav",
        str(dst),
    ]
    return argv


def concat_working_copy(
    sources: Sequence[Path], dst: Path, *, sample_rate: int = 16000, normalize: bool
) -> None:
    """Join `sources` (adjacent, in order) into one working copy at `dst`. See
    `build_concat_argv` for what `normalize` costs you if you get it wrong."""
    if not sources:
        msg = "concat_working_copy needs at least one source"
        raise ValueError(msg)
    dst.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        build_concat_argv(sources, dst, sample_rate=sample_rate, normalize=normalize),
        check=True,
    )


def build_slice_argv(src: Path, dst: Path, start: float, end: float) -> list[str]:
    """ffmpeg argv to extract the [start, end] second window of `src`."""
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
    """Extract the [start, end] second window of `src` into `dst`."""
    dst.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(build_slice_argv(src, dst, start, end), check=True)


@contextmanager
def scratch_wav(path: Path) -> Iterator[Path]:
    """Yield `path` for a transient working clip, deleting it on exit.

    The batch passes (refine/ingest/identify) decode a working copy per segment and
    slice a clip per turn, consume each immediately (transcribe/diarize/embed), then
    never read it again. Wrapping every clip in this keeps the shared `work/` dir
    from growing without bound — the files are scratch, not output.
    """
    try:
        yield path
    finally:
        path.unlink(missing_ok=True)


def decode_pcm_f32(audio: Path, *, sample_rate: int = 16000) -> np.ndarray:
    """Decode `audio` to a 1-D mono float32 waveform at `sample_rate`, via ffmpeg.

    Feeds Whisper feature extractors directly, bypassing the `datasets`/torchcodec
    audio backend, which fails to load its shared libs on this torch stack (the
    same decode path pyannote can't take). Samples are in [-1, 1].
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


def combine_result(result: AsrResult) -> tuple[str, float | None]:
    """Join a result's segments into one text and a mean confidence.

    Used when a whole clip (e.g. one speaker turn) should become a single
    transcript row.
    """
    text = " ".join(s.text.strip() for s in result.segments if s.text.strip())
    confidences = [s.confidence for s in result.segments]
    return text.strip(), (mean(confidences) if confidences else None)


def result_to_drafts(
    result: AsrResult, *, segment_start: datetime, model_name: str
) -> list[TranscriptDraft]:
    """Map a clip-relative `AsrResult` to absolute-time transcript drafts."""
    drafts: list[TranscriptDraft] = []
    for segment in result.segments:
        text = segment.text.strip()
        if not text:
            continue
        drafts.append(
            TranscriptDraft(
                start=segment_start + timedelta(seconds=segment.start),
                end=segment_start + timedelta(seconds=segment.end),
                text=text,
                language=result.language,
                language_confidence=result.language_confidence,
                asr_confidence=segment.confidence,
                asr_model=model_name,
            )
        )
    return drafts


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


def mlx_transcribe(
    audio: Path,
    *,
    model: str = DEFAULT_MODEL,
    language: str | None = None,
    words: bool = False,
    initial_prompt: str | None = None,
) -> AsrResult:
    """Transcribe `audio` with mlx-whisper (Apple-Silicon native). Lazy import.

    `language` forces a language (e.g. "en"/"nl"); None auto-detects. `words=True`
    adds per-word timings (for aligning a whole-segment transcription to diarized
    speakers) at some extra cost; off by default. `initial_prompt` biases decoding
    toward the household vocabulary (recall.vocabulary) — names it has seen in the
    prompt get spelled right.
    """
    import mlx_whisper  # noqa: PLC0415 - lazy: mlx-whisper is an optional heavy dep

    # Anti-hallucination decoding. condition_on_previous_text=False stops a
    # repetition loop from feeding itself across windows; the temperature
    # fallback re-decodes a window whose output trips the compression-ratio
    # (repetition) or logprob (gibberish) thresholds. These only work when the
    # input has real context — hence we transcribe whole segments, never tiny
    # isolated slices.
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
