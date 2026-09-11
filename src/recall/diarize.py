"""Speaker diarization: who spoke when, within one audio clip.

Diarization splits a clip into speaker-homogeneous *turns* with relative labels
(SPEAKER_00, SPEAKER_01, ...). Identification (mapping those labels to named
household members) is a separate later step. The real pyannote pipeline is
isolated behind a lazy import; the `Diarizer` protocol lets orchestration run
with a stub.
"""

from __future__ import annotations

import os
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Protocol

DEFAULT_DIARIZER = "pyannote/speaker-diarization-3.1"


@dataclass(frozen=True)
class SpeakerTurn:
    """A contiguous span attributed to one (relative) speaker, clip-relative."""

    speaker: str
    start: float
    end: float


class Diarizer(Protocol):
    """Anything that splits an audio file into speaker turns."""

    def __call__(self, audio: Path, /) -> list[SpeakerTurn]: ...


def tuned_parameters(
    current: Mapping[str, Any],
    *,
    threshold: float | None,
    min_cluster_size: int | None,
) -> dict[str, Any]:
    """`current` with the clustering overrides applied, as a fresh dict.

    Both None returns an equal copy, so the default path cannot change what production
    diarizes. Copied rather than edited in place because pyannote hands back the
    pipeline's live parameter dict, and mutating it would retune every later call in the
    process — including the refine daemon's.
    """
    if threshold is None and min_cluster_size is None:
        return {k: dict(v) if isinstance(v, Mapping) else v for k, v in current.items()}
    clustering = current.get("clustering")
    if not isinstance(clustering, Mapping):
        msg = f"pipeline has no clustering parameters to tune: {sorted(current)}"
        raise ValueError(msg)
    tuned = {k: dict(v) if isinstance(v, Mapping) else v for k, v in current.items()}
    if threshold is not None:
        tuned["clustering"]["threshold"] = threshold
    if min_cluster_size is not None:
        tuned["clustering"]["min_cluster_size"] = min_cluster_size
    return tuned


#: Loaded pipelines, keyed by model AND tuning. Keyed on the tuning too because
#: `instantiate` MUTATES the pipeline: a cache keyed on the model alone would hand
#: a later default-parameter call whatever the last tuned call left behind, and the
#: default path is production. Two tunings cost two pipelines, which is the honest
#: price of asking for two.
_PIPELINE_CACHE: dict[tuple[str, float | None, int | None], object] = {}


def _pipeline(
    model: str,
    token: str | None,
    threshold: float | None,
    min_cluster_size: int | None,
) -> object:
    """Load (once per process) the diarization pipeline. Heavy + gated.

    Mirrors `speakerid._inference`. Without this every call paid the load again —
    seconds of weights per segment, which is the bill the shim architecture
    (docs/architecture.md, stage E2) exists to stop paying. It did not matter while
    the caller was a one-shot CLI pass; it matters as soon as a long-lived shim is
    the caller.
    """
    key = (model, threshold, min_cluster_size)
    cached = _PIPELINE_CACHE.get(key)
    if cached is not None:
        return cached
    from pyannote.audio import Pipeline  # noqa: PLC0415 - lazy heavy/gated dep

    pipeline = Pipeline.from_pretrained(model, token=token)
    if pipeline is None:
        msg = f"could not load diarization pipeline {model!r} (HF token/terms?)"
        raise RuntimeError(msg)
    if threshold is not None or min_cluster_size is not None:
        pipeline.instantiate(
            tuned_parameters(
                pipeline.parameters(instantiated=True),
                threshold=threshold,
                min_cluster_size=min_cluster_size,
            )
        )
    _PIPELINE_CACHE[key] = pipeline
    return pipeline


def pyannote_diarize(
    audio: Path,
    *,
    model: str = DEFAULT_DIARIZER,
    hf_token: str | None = None,
    clustering_threshold: float | None = None,
    min_cluster_size: int | None = None,
) -> list[SpeakerTurn]:
    """Diarize `audio` with pyannote.audio (PyTorch/MPS). Lazy import.

    The pyannote model is gated on Hugging Face; `hf_token` (defaulting to the
    HF_TOKEN env var) must have accepted its terms.

    `clustering_threshold` / `min_cluster_size` override pyannote's shipped clustering
    parameters (0.7046 / 12, tuned on meeting corpora). Both default to None = leave the
    pipeline exactly as shipped, so production is unaffected until a measurement says a
    different value is better.

    Measured on household audio, `min_cluster_size` is the knob that matters. It counts
    10 s windows, so a short second speaker inside a 60 s segment can never reach 12 and
    is absorbed into the dominant speaker's cluster — a cluster that straddles the
    handover, which is how the first words of a reply get attributed to whoever spoke
    before. Dropping it to 3 raised a segment from 6 clusters to 8. The threshold does
    NOT work the way its name suggests: the shipped value sits near a cluster-count
    maximum and moving it in either direction merges (0.40 and 0.90 both collapse that
    same segment to 2).
    """
    import torch  # noqa: PLC0415 - heavy

    from recall.asr import decode_pcm_f32  # noqa: PLC0415 - heavy/optional path

    token = hf_token or os.environ.get("HF_TOKEN")
    pipeline = _pipeline(model, token, clustering_threshold, min_cluster_size)
    # pyannote 4.x decodes files via torchcodec, which won't load on this torch
    # stack; hand it an ffmpeg-decoded in-memory waveform instead, like the
    # embedding path does (recall.speakerid._decode_mono).
    rate = 16000
    samples = decode_pcm_f32(audio, sample_rate=rate).copy()
    waveform = torch.from_numpy(samples).unsqueeze(0)
    # pyannote is untyped, so the cached pipeline arrives here as an `object` and
    # calling it needs the same ignore `speakerid.pyannote_embed` uses.
    result = pipeline({"waveform": waveform, "sample_rate": rate})  # type: ignore[operator]
    # pyannote 4.x returns a DiarizeOutput; its exclusive (non-overlapping)
    # diarization is the clean one for per-turn transcript attribution. Older
    # pyannote returns an Annotation directly.
    # Any: the attribute exists on pyannote 4.x's DiarizeOutput and not on the bare
    # Annotation older versions return, so the union has no common statically-known
    # `itertracks` for mypy to see.
    annotation: Any = getattr(result, "exclusive_speaker_diarization", result)
    turns = [
        SpeakerTurn(
            speaker=str(speaker),
            start=float(segment.start),
            end=float(segment.end),
        )
        for segment, _, speaker in annotation.itertracks(yield_label=True)
    ]
    turns.sort(key=lambda t: t.start)
    return turns
