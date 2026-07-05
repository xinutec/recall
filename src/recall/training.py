"""Export the corrections corpus as a fine-tuning dataset.

Each human correction (recall.review) becomes a training example: the audio span
is sliced from the retained raw segment and paired with the corrected text and
language. The result is a `clips/` directory plus a JSONL `manifest.jsonl` that a
LoRA fine-tune (recall.finetune) consumes. This export is the testable bridge
between the correction website and model improvement; training itself needs the
heavy ML environment.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path

from recall.asr import slice_clip
from recall.store import Store
from recall.wer import normalize_text

# A corpus needs at least this many examples to split into two non-empty sides.
_MIN_FOR_SPLIT = 2

# ASR-label hygiene. Whisper trains on 30s windows; a sub-2s clip is mostly
# padding silence, and a corpus dominated by them teaches early-EOS (the
# adapter-truncation regression, measured on real audio). Few-word backchannels
# ("Yeah. Okay.") add the same risk with no lexical value. Their text stays in
# the corrections table — they just don't become training labels.
_MIN_CLIP_SECONDS = 2.0
_MIN_CLIP_WORDS = 4


def split_examples[T](
    examples: list[T], *, holdout: float = 0.2
) -> tuple[list[T], list[T]]:
    """Deterministically partition a corpus into (train, test).

    Holds out roughly `holdout` of the examples by a fixed stride, so the same
    corpus always yields the same split (reproducible eval). Guarantees both
    sides are non-empty for any corpus of two or more examples.
    """
    if len(examples) < _MIN_FOR_SPLIT:
        return list(examples), []
    stride = max(_MIN_FOR_SPLIT, round(1.0 / holdout))
    train: list[T] = []
    test: list[T] = []
    for index, example in enumerate(examples):
        (test if index % stride == 0 else train).append(example)
    if not train:  # tiny corpus where every index hit the stride
        train, test = test, train
    return train, test


@dataclass(frozen=True)
class TrainingExample:
    """One labelled (audio clip -> correct text) pair."""

    audio: str
    text: str
    language: str | None


def export_corpus(store: Store, dest: Path) -> int:
    """Write the corrections corpus to `dest` (clips/ + manifest.jsonl).

    Returns the number of examples written. Corrections whose audio is missing
    are skipped.
    """
    clips = dest / "clips"
    clips.mkdir(parents=True, exist_ok=True)

    examples: list[TrainingExample] = []
    skipped_short = 0
    skipped_dupe = 0
    # (audio segment, normalized text) of everything exported so far — an
    # overlapping re-correction of the same words must not weight the corpus twice.
    seen: set[tuple[int, str]] = set()
    for correction in store.corrections():
        if correction.audio_segment_id is None:
            continue
        ref = store.audio_segment_ref(correction.audio_segment_id)
        if ref is None:
            continue
        duration = (correction.end - correction.start).total_seconds()
        words = len(correction.corrected_text.split())
        if duration < _MIN_CLIP_SECONDS or words < _MIN_CLIP_WORDS:
            skipped_short += 1
            continue
        key = (correction.audio_segment_id, normalize_text(correction.corrected_text))
        if key in seen:
            skipped_dupe += 1
            continue
        seen.add(key)
        path, audio_start = ref
        rel_start = max(0.0, (correction.start - audio_start).total_seconds())
        rel_end = (correction.end - audio_start).total_seconds()
        clip = clips / f"{correction.id:06d}.wav"
        slice_clip(Path(path), clip, rel_start, rel_end)
        examples.append(
            TrainingExample(
                audio=str(clip),
                text=correction.corrected_text,
                language=correction.language,
            )
        )
    if skipped_short or skipped_dupe:
        print(
            f"[export] {len(examples)} examples; skipped {skipped_short} "
            f"backchannel/short clips and {skipped_dupe} overlapping duplicates"
        )

    manifest = dest / "manifest.jsonl"
    with manifest.open("w", encoding="utf-8") as handle:
        for example in examples:
            handle.write(
                json.dumps(
                    {
                        "audio": example.audio,
                        "text": example.text,
                        "language": example.language,
                    }
                )
                + "\n"
            )
    return len(examples)
