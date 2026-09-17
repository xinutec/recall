"""The speaker-embedding model, behind a lazy import.

⚠ **What is left here is the MODEL and nothing else.** The matching arithmetic —
cosine, the profile type, the threshold rule — lived here until 2026-09-17 and is
now `recalld::identify`, on the side that owns the voiceprints. This module is
part of the Python floor: pyannote's weights, the decode, and the process that
holds them, reached through `shim_voices`.
"""

from __future__ import annotations

import os
from collections.abc import Sequence
from pathlib import Path
from typing import Protocol

Embedding = Sequence[float]


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
