"""What a candidate segment *contains* — the veto the cleanup rests on.

Volume says how loud a minute was. It cannot say what was in it, and that is the only
question a deletion actually turns on. Two signals answer it, and they are not equals:

* **The VAD decides.** Silero is a trained speech detector, already trusted in this
  pipeline to gate transcription. On the segments this archive nearly lost it is
  unambiguous: it found speech in every one of the far-field Dutch minutes whose
  60-second
  mean sat below the noise-floor threshold, and it calls the coughing and the shuffling
  in
  a genuinely empty span exactly what they are — not speech. A span holding *any*
  detected
  speech is never offered for deletion.

* **Structure ranks.** How far a segment departs from its own mic's noise fingerprint
  (recall.spectrum) sorts dead air above a room with someone moving in it. It is a good
  signal and a bad judge: measured here, idle noise reaches 0.92 and real speech drops
  to
  0.73, so it overlaps and gets no vote on what is safe.

Why not the transcript? It was the veto, and it failed. A reprocessing pass hides the
turns
it replaces, so a segment of real Dutch ("ik moet niet zeggen", "zelfs op de vorm van
60-70
minuten") ended up with no *visible* turn at all, and a rule that counted visible turns
saw
an empty minute. Bookkeeping about a transcript is not evidence about audio. The audio
is.
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

from recall.quiet import SWEEPABLE_KINDS
from recall.spectrum import band_shapes, decode_shape, structure
from recall.store import Store
from recall.vad import Vad, silero_speech_regions


@dataclass(frozen=True)
class Analysis:
    """What one segment turned out to hold."""

    speech_s: float
    structure: float | None


def analyse_segment(path: Path, vad: Vad, reference: bytes | None) -> Analysis | None:
    """Listen to one segment: how much speech is in it, and how unlike the mic's own
    idle noise it is. None if it will not decode — unknown, and so never swept."""
    regions = vad(path)
    speech_s = sum(region.end - region.start for region in regions)

    novelty: float | None = None
    if reference is not None:
        shapes = band_shapes(path)
        if shapes is not None:
            novelty = structure(shapes, decode_shape(reference))
    return Analysis(speech_s=speech_s, structure=novelty)


def analyse_segments(
    store: Store,
    *,
    vad: Vad | None = None,
    batch: int = 200,
    should_stop: Callable[[], bool] | None = None,
) -> int:
    """Analyse the segments a cleanup could act on: quiet by volume, and read by ASR.

    Only those — a loud segment can never enter a span, so running a speech detector
    over
    it would be an hour of compute spent on a foregone conclusion. Cached per segment
    and
    resumable, like the envelope scan: this is ~0.6s of model per minute of audio and it
    is never paid twice.
    """
    # Resolved here, not bound as a default: the module function must stay patchable.
    detector = vad if vad is not None else silero_speech_regions
    shapes = {
        source_id: store.source_noise_shape(source_id)
        for source_id in store.sweepable_source_ids()
    }
    analysed = 0
    for audio_id, path, source_id in store.audio_segments_to_analyse(
        kinds=SWEEPABLE_KINDS, limit=batch
    ):
        if should_stop is not None and should_stop():
            break
        result = analyse_segment(Path(path), detector, shapes.get(source_id))
        if result is None:
            continue
        store.set_audio_analysis(audio_id, result.speech_s, result.structure)
        analysed += 1
    return analysed
