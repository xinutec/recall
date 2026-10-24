"""The speaker-embedding model, behind a lazy import. Matching vectors to
voices is `recalld::identify`.
"""

from __future__ import annotations

import os
from pathlib import Path

_EMBED_RATE = 16000

#: Where embeddings run: the CPU, not the Apple GPU the diarizer uses. Every
#: span has its own length, and the GPU recompiles for each new shape:
#: measured 0.015 s a span on the CPU against 0.49 s on the GPU, and about
#: 10 s in production with the GPU also diarizing.
EMBED_DEVICE = "cpu"


def _decode_mono(audio: Path, rate: int = _EMBED_RATE) -> object:
    """`audio` as a mono float32 tensor of shape (1, samples), decoded here
    because pyannote's own decoder (torchcodec) does not load on this stack."""
    import torch  # noqa: PLC0415 - heavy

    from recall.asr import decode_pcm_f32  # noqa: PLC0415 - heavy/optional path

    return torch.from_numpy(decode_pcm_f32(audio, sample_rate=rate).copy()).unsqueeze(0)


_INFERENCE_CACHE: dict[str, object] = {}


def _inference(model: str, token: str | None) -> object:
    """The pyannote embedding inference, loaded once per process."""
    cached = _INFERENCE_CACHE.get(model)
    if cached is not None:
        return cached
    import torch  # noqa: PLC0415 - heavy
    from pyannote.audio import Inference, Model  # noqa: PLC0415 - lazy heavy/gated

    embedding_model = Model.from_pretrained(model, token=token)
    if embedding_model is None:
        msg = f"could not load embedding model {model!r} (HF token/terms?)"
        raise RuntimeError(msg)
    inference = Inference(embedding_model, window="whole")
    inference.to(torch.device(EMBED_DEVICE))
    _INFERENCE_CACHE[model] = inference
    return inference


def pyannote_embed(
    audio: Path, *, model: str = "pyannote/embedding", hf_token: str | None = None
) -> list[float]:
    """Embed a clip with pyannote. The model is gated on Hugging Face;
    `hf_token` defaults to `HF_TOKEN`."""
    token = hf_token or os.environ.get("HF_TOKEN")
    inference = _inference(model, token)
    waveform = _decode_mono(Path(audio))
    vector = inference({"waveform": waveform, "sample_rate": _EMBED_RATE})  # type: ignore[operator]  # pyannote's Inference is untyped
    return [float(x) for x in vector]
