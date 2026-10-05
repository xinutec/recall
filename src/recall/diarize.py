"""Speaker diarization: who spoke when within one clip, as turns labelled
SPEAKER_00, SPEAKER_01, ... Naming them happens on the server. pyannote is
imported lazily.
"""

from __future__ import annotations

import os
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import Any

DEFAULT_DIARIZER = "pyannote/speaker-diarization-3.1"


@dataclass(frozen=True)
class SpeakerTurn:
    """One speaker's span, timed from the start of the clip."""

    speaker: str
    start: float
    end: float


def tuned_parameters(
    current: Mapping[str, Any],
    *,
    threshold: float | None,
    min_cluster_size: int | None,
) -> dict[str, Any]:
    """`current` with the clustering overrides applied, as a new dict; with
    both None, an equal copy. A copy because pyannote returns the pipeline's
    live dict, and editing it would retune every later call.
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


#: Loaded pipelines, by model and tuning: `instantiate` changes a pipeline in
#: place, so keyed on the model alone a default call would get the last tuning.
_PIPELINE_CACHE: dict[tuple[str, float | None, int | None], object] = {}


def _pipeline(
    model: str,
    token: str | None,
    threshold: float | None,
    min_cluster_size: int | None,
) -> object:
    """The diarization pipeline, loaded once per process and tuning."""
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
    """Diarize `audio` with pyannote. The model is gated on Hugging Face;
    `hf_token` (default `HF_TOKEN`) must have accepted its terms.

    `clustering_threshold` and `min_cluster_size` override pyannote's shipped
    0.7046 and 12; None keeps them.

    On household audio `min_cluster_size` is the one that matters: it counts
    10 s windows, so a short second speaker in a 60 s clip never reaches 12 and
    is absorbed by the dominant one, which is how a reply's first words go to
    the previous speaker. At 3, one clip went from 6 clusters to 8. The
    threshold sits near a maximum of cluster count: 0.40 and 0.90 both collapse
    that clip to 2.
    """
    import torch  # noqa: PLC0415 - heavy

    from recall.asr import decode_pcm_f32  # noqa: PLC0415 - heavy/optional path

    token = hf_token or os.environ.get("HF_TOKEN")
    pipeline = _pipeline(model, token, clustering_threshold, min_cluster_size)
    # Decoded here: pyannote's own decoder (torchcodec) does not load on this
    # torch stack.
    rate = 16000
    samples = decode_pcm_f32(audio, sample_rate=rate).copy()
    waveform = torch.from_numpy(samples).unsqueeze(0)
    result = pipeline({"waveform": waveform, "sample_rate": rate})  # type: ignore[operator]  # pyannote is untyped
    # pyannote 4.x returns a DiarizeOutput, whose exclusive (non-overlapping)
    # diarization is used; older versions return the Annotation itself.
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
