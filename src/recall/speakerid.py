"""Speaker identification: match a voice embedding to an enrolled person.

Enrollment stores reference voiceprints per household member (recall.store). At
runtime each speaker turn is embedded and compared (cosine) to every profile; the
best match above a threshold names the person, otherwise it's "unknown". The
embedding model itself is heavy/gated and isolated behind a lazy import; the
matching logic here is pure and fully tested.
"""

from __future__ import annotations

import math
import os
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Protocol

Embedding = Sequence[float]


def cosine_similarity(a: Embedding, b: Embedding) -> float:
    """Cosine similarity in [-1, 1]; 0.0 if either vector is zero-length."""
    if len(a) != len(b):
        msg = f"embedding dimension mismatch: {len(a)} vs {len(b)}"
        raise ValueError(msg)
    dot = sum(x * y for x, y in zip(a, b, strict=True))
    norm_a = math.sqrt(sum(x * x for x in a))
    norm_b = math.sqrt(sum(y * y for y in b))
    if norm_a == 0.0 or norm_b == 0.0:
        return 0.0
    return dot / (norm_a * norm_b)


@dataclass(frozen=True)
class SpeakerProfile:
    """An enrolled person and their reference voiceprints."""

    name: str
    embeddings: tuple[tuple[float, ...], ...]

    def similarity(self, embedding: Embedding) -> float:
        """Best (max) similarity of `embedding` to any enrolled voiceprint."""
        return max(
            (cosine_similarity(embedding, ref) for ref in self.embeddings),
            default=0.0,
        )


def identify(
    embedding: Embedding,
    profiles: Sequence[SpeakerProfile],
    *,
    threshold: float,
) -> str | None:
    """Name the best-matching profile above `threshold`, else None (unknown)."""
    best_name: str | None = None
    best_similarity = -1.0
    for profile in profiles:
        similarity = profile.similarity(embedding)
        if similarity > best_similarity:
            best_similarity = similarity
            best_name = profile.name
    return best_name if best_similarity >= threshold else None


class Embedder(Protocol):
    """Anything that turns an audio clip into a speaker embedding."""

    def __call__(self, audio: Path, /) -> list[float]: ...


_EMBED_RATE = 16000


def _decode_mono(audio: Path, rate: int = _EMBED_RATE) -> object:
    """Decode `audio` to a mono float32 waveform tensor of shape (1, samples).

    Done via ffmpeg (like loudness.speech_level) rather than letting pyannote
    decode the file itself: its torchcodec backend fails to load on this stack,
    so we hand it an in-memory waveform instead — the decode path it can't break.
    """
    import subprocess  # noqa: PLC0415 - local to the heavy/optional path

    import numpy as np  # noqa: PLC0415 - heavy
    import torch  # noqa: PLC0415 - heavy

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
            str(rate),
            "-f",
            "s16le",
            "-",
        ],
        capture_output=True,
        check=True,
    ).stdout
    samples = np.frombuffer(pcm, dtype=np.int16).astype(np.float32) / 32768.0
    return torch.from_numpy(samples.copy()).unsqueeze(0)


_INFERENCE_CACHE: dict[str, object] = {}


def _inference(model: str, token: str | None) -> object:
    """Load (once per process) the pyannote embedding inference. Heavy + gated."""
    cached = _INFERENCE_CACHE.get(model)
    if cached is not None:
        return cached
    from pyannote.audio import Inference, Model  # noqa: PLC0415 - lazy heavy/gated

    embedding_model = Model.from_pretrained(model, token=token)
    if embedding_model is None:
        msg = f"could not load embedding model {model!r} (HF token/terms?)"
        raise RuntimeError(msg)
    inference = Inference(embedding_model, window="whole")
    _INFERENCE_CACHE[model] = inference
    return inference


def pyannote_embed(
    audio: Path, *, model: str = "pyannote/embedding", hf_token: str | None = None
) -> list[float]:
    """Embed a clip with pyannote (PyTorch/MPS). Lazy import; gated on HF.

    The model loads once per process and is reused (cached), so repeated calls —
    the labelling suggest path and the enrolment backfill — stay fast.
    `hf_token` defaults to the HF_TOKEN env var.
    """
    token = hf_token or os.environ.get("HF_TOKEN")
    inference = _inference(model, token)
    waveform = _decode_mono(Path(audio))
    vector = inference({"waveform": waveform, "sample_rate": _EMBED_RATE})  # type: ignore[operator]
    return [float(x) for x in vector]
