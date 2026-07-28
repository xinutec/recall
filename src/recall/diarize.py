"""Speaker diarization: who spoke when, within one audio clip.

Diarization splits a clip into speaker-homogeneous *turns* with relative labels
(SPEAKER_00, SPEAKER_01, ...). Identification (mapping those labels to named
household members) is a separate later step. The real pyannote pipeline is
isolated behind a lazy import; the `Diarizer` protocol lets orchestration run
with a stub.
"""

from __future__ import annotations

import os
from dataclasses import dataclass
from pathlib import Path
from typing import Protocol

DEFAULT_DIARIZER = "pyannote/speaker-diarization-3.1"


@dataclass(frozen=True)
class SpeakerTurn:
    """A contiguous span attributed to one (relative) speaker, clip-relative."""

    speaker: str
    start: float
    end: float


@dataclass(frozen=True)
class Diarization:
    """One clip's turns in both views pyannote produces.

    `exclusive` gives one speaker per instant: where two people talk at once, the
    dominant voice takes the whole stretch and the other is erased. `overlapping`
    keeps both, so a moment of simultaneous speech appears under each speaker.

    Word alignment wants both, because a handover *is* a moment of overlap — the
    incoming speaker starts before the outgoing one stops. The exclusive view is
    precisely where that evidence has been thrown away, so on its own it hands the
    incoming speaker's first words to whoever was talking before.
    """

    exclusive: tuple[SpeakerTurn, ...]
    overlapping: tuple[SpeakerTurn, ...]


class Diarizer(Protocol):
    """Anything that splits an audio file into speaker turns."""

    def __call__(self, audio: Path, /) -> Diarization: ...


def _turns(annotation: object) -> tuple[SpeakerTurn, ...]:
    """A pyannote `Annotation`'s tracks as sorted `SpeakerTurn`s."""
    turns = [
        SpeakerTurn(
            speaker=str(speaker),
            start=float(segment.start),
            end=float(segment.end),
        )
        for segment, _, speaker in annotation.itertracks(yield_label=True)  # type: ignore[attr-defined]
    ]
    turns.sort(key=lambda t: (t.start, t.end, t.speaker))
    return tuple(turns)


def pyannote_diarize(
    audio: Path, *, model: str = DEFAULT_DIARIZER, hf_token: str | None = None
) -> Diarization:
    """Diarize `audio` with pyannote.audio (PyTorch/MPS). Lazy import.

    The pyannote model is gated on Hugging Face; `hf_token` (defaulting to the
    HF_TOKEN env var) must have accepted its terms.
    """
    import torch  # noqa: PLC0415 - heavy
    from pyannote.audio import Pipeline  # noqa: PLC0415 - lazy heavy/gated dep

    from recall.asr import decode_pcm_f32  # noqa: PLC0415 - heavy/optional path

    token = hf_token or os.environ.get("HF_TOKEN")
    pipeline = Pipeline.from_pretrained(model, token=token)
    if pipeline is None:
        msg = f"could not load diarization pipeline {model!r} (HF token/terms?)"
        raise RuntimeError(msg)
    # pyannote 4.x decodes files via torchcodec, which won't load on this torch
    # stack; hand it an ffmpeg-decoded in-memory waveform instead, like the
    # embedding path does (recall.speakerid._decode_mono).
    rate = 16000
    samples = decode_pcm_f32(audio, sample_rate=rate).copy()
    waveform = torch.from_numpy(samples).unsqueeze(0)
    result = pipeline({"waveform": waveform, "sample_rate": rate})
    # pyannote 4.x returns a DiarizeOutput carrying both views; older pyannote
    # returns a bare Annotation, in which case the two views coincide and the
    # overlap-aware alignment rule degrades to the exclusive one.
    return Diarization(
        exclusive=_turns(getattr(result, "exclusive_speaker_diarization", result)),
        overlapping=_turns(getattr(result, "speaker_diarization", result)),
    )
