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
from collections import defaultdict
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path

from recall.asr import slice_clip
from recall.store import Store
from recall.store_models import Correction
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

# Stitching. Whisper wants ~30s of context; isolated 2-5s clips are what taught
# the early-EOS regression. Adjacent same-speaker corrections within one audio
# segment are merged into a single <=30s window, so the label reads as continuous
# speech. Two guards keep the merge honest (the audio must contain only the
# labelled words): a small positive gap (a within-speaker micro-pause, too short
# to hide another speaker's interjection or uncorrected speech) and the same
# named voice on both sides. Overlapping spans (gap < 0) are re-corrections of the
# same words, not a sequence, so they fall through to dedup and never stitch.
_MAX_WINDOW_SECONDS = 30.0
_MAX_STITCH_GAP_SECONDS = 0.4


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


@dataclass(frozen=True)
class _Window:
    """One training window: a run of stitched same-speaker corrections."""

    audio_segment_id: int
    path: str
    audio_start: datetime
    corrections: tuple[Correction, ...]

    @property
    def start(self) -> datetime:
        return self.corrections[0].start

    @property
    def end(self) -> datetime:
        return self.corrections[-1].end

    @property
    def text(self) -> str:
        return " ".join(c.corrected_text for c in self.corrections)

    @property
    def language(self) -> str | None:
        return next((c.language for c in self.corrections if c.language), None)


def _stitch_windows(
    corrections: list[Correction], path: str, audio_start: datetime
) -> list[_Window]:
    """Greedily merge adjacent same-speaker corrections into ≤30s windows.

    `corrections` are all from one audio segment, sorted by start. A run extends
    while the next turn is the same voice, follows within a small positive gap,
    and keeps the window under 30s; anything else opens a new run.
    """
    windows: list[_Window] = []
    run: list[Correction] = [corrections[0]]
    for nxt in corrections[1:]:
        gap = (nxt.start - run[-1].end).total_seconds()
        span = (nxt.end - run[0].start).total_seconds()
        same_voice = nxt.speaker == run[0].speaker
        if (
            0.0 <= gap <= _MAX_STITCH_GAP_SECONDS
            and same_voice
            and span <= _MAX_WINDOW_SECONDS
        ):
            run.append(nxt)
        else:
            windows.append(
                _Window(run[0].audio_segment_id or 0, path, audio_start, tuple(run))
            )
            run = [nxt]
    windows.append(_Window(run[0].audio_segment_id or 0, path, audio_start, tuple(run)))
    return windows


def export_corpus(store: Store, dest: Path) -> int:
    """Write the corrections corpus to `dest` (clips/ + manifest.jsonl).

    Adjacent same-speaker corrections are stitched into one ≤30s window before
    the hygiene gate, so a short turn between two contentful ones survives as
    context instead of being dropped. Returns the number of examples written;
    corrections whose audio is missing are skipped.
    """
    clips = dest / "clips"
    clips.mkdir(parents=True, exist_ok=True)

    # Group resolvable corrections by audio segment, then stitch each group.
    by_segment: dict[int, list[Correction]] = defaultdict(list)
    refs: dict[int, tuple[str, datetime]] = {}
    for correction in store.corrections():
        if correction.audio_segment_id is None:
            continue
        ref = store.audio_segment_ref(correction.audio_segment_id)
        if ref is None:
            continue
        by_segment[correction.audio_segment_id].append(correction)
        refs[correction.audio_segment_id] = ref

    windows: list[_Window] = []
    for seg_id in sorted(by_segment):
        group = sorted(by_segment[seg_id], key=lambda c: (c.start, c.id))
        path, audio_start = refs[seg_id]
        windows.extend(_stitch_windows(group, path, audio_start))

    examples: list[TrainingExample] = []
    skipped_short = 0
    skipped_dupe = 0
    # (audio segment, normalized text) of everything exported so far — an
    # overlapping re-correction of the same words must not weight the corpus twice.
    seen: set[tuple[int, str]] = set()
    for window in windows:
        duration = (window.end - window.start).total_seconds()
        words = len(window.text.split())
        if duration < _MIN_CLIP_SECONDS or words < _MIN_CLIP_WORDS:
            skipped_short += 1
            continue
        key = (window.audio_segment_id, normalize_text(window.text))
        if key in seen:
            skipped_dupe += 1
            continue
        seen.add(key)
        rel_start = max(0.0, (window.start - window.audio_start).total_seconds())
        rel_end = (window.end - window.audio_start).total_seconds()
        clip = clips / f"{window.corrections[0].id:06d}.wav"
        slice_clip(Path(window.path), clip, rel_start, rel_end)
        examples.append(
            TrainingExample(
                audio=str(clip),
                text=window.text,
                language=window.language,
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
