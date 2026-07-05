"""Human review and correction — the active-learning flywheel's logic.

A correction does two things at once:
1. supersedes the low-confidence transcript with a human-authored, ground-truth
   segment (so search/recall is immediately right), and
2. records the (audio span -> correct text) pair in `corrections` — the labelled
   corpus a later LoRA fine-tune consumes to improve the model (pipeline.md §6).
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime

from recall.store import (
    HUMAN_MODEL,
    Store,
    TranscriptSegment,
    human_correction_provenance,
)

HUMAN_CONFIDENCE = 1.0

__all__ = [
    "HUMAN_MODEL",
    "SpeakerFragment",
    "apply_correction",
    "review_queue",
    "split_correction",
]


@dataclass(frozen=True)
class SpeakerFragment:
    """One single-speaker piece of a split turn: an audio span + text + speaker."""

    start: datetime
    end: datetime
    text: str
    speaker: str | None


def review_queue(
    store: Store, *, max_confidence: float = 0.9, limit: int = 50
) -> list[TranscriptSegment]:
    """The segments most worth a human's attention (lowest confidence first)."""
    return store.low_confidence_segments(max_confidence=max_confidence, limit=limit)


def apply_correction(  # noqa: PLR0913 - one kwarg per labelled field
    store: Store,
    segment_id: int,
    corrected_text: str,
    *,
    now: datetime,
    speaker: str | None = None,
    start: datetime | None = None,
    end: datetime | None = None,
    language: str | None = None,
) -> int:
    """Replace `segment_id` with a human-authored segment; record the pair.

    `speaker` (a name) tags who said it; `start`/`end` override the audio span
    (the boundary editor trims a clip to exactly one speaker); `language` corrects
    a mis-detected language (e.g. Dutch heard as English) — defaults to the old
    one. All flow into the labelled corpus so the fine-tune data is per-person,
    tightly aligned, and language-correct. Raises if the segment is missing/blank.
    """
    text = corrected_text.strip()
    if not text:
        msg = "corrected text must not be blank"
        raise ValueError(msg)

    old = store.get_transcript(segment_id)
    if old is None:
        msg = f"no transcript segment with id {segment_id}"
        raise ValueError(msg)
    if old.superseded_by is not None:
        # A double-tap / second-tab correcting a stale id would mint a SECOND
        # current human turn and a duplicate corpus pair.
        msg = f"turn #{segment_id} was already superseded - correct the current version"
        raise ValueError(msg)

    lang = language if language is not None else old.language
    span_start = start if start is not None else old.start
    span_end = end if end is not None else old.end
    new_id = store.add_transcript_segment(
        audio_segment_id=old.audio_segment_id,
        start=span_start,
        end=span_end,
        text=text,
        asr_model=HUMAN_MODEL,
        language=lang,
        language_confidence=old.language_confidence,
        asr_confidence=HUMAN_CONFIDENCE,
        speaker_label=speaker if speaker is not None else old.speaker_label,
        speaker_id=old.speaker_id,
        # Carry the diarization voice forward, so correcting a turn's text keeps it
        # attributed to its voice (and named, in a session) instead of going "unknown".
        speaker_cluster=old.speaker_cluster,
        provenance=human_correction_provenance(old.id),
        created=now,
    )
    store.supersede(old.id, new_id)
    store.add_correction(
        transcript_segment_id=old.id,
        audio_segment_id=old.audio_segment_id,
        start=span_start,
        end=span_end,
        original_text=old.text,
        corrected_text=text,
        language=lang,
        created=now,
        speaker=speaker,
        # The clip's audio quality, carried onto the pair: a human-readable label on
        # faint audio is still good ASR data, but too degraded to enrol as a voice.
        audio_confidence=old.asr_confidence,
    )
    return new_id


def split_correction(
    store: Store,
    segment_id: int,
    fragments: list[SpeakerFragment],
    *,
    now: datetime,
) -> list[int]:
    """Replace one turn with several single-speaker human fragments.

    Each fragment becomes a human turn + a labelled corpus pair (with its speaker
    and exact span); the original is superseded and lineage is recorded. This is
    the per-speaker refinement: turn a two-voice turn into clean, tagged pairs.
    """
    if not fragments:
        msg = "at least one fragment is required"
        raise ValueError(msg)
    old = store.get_transcript(segment_id)
    if old is None:
        msg = f"no transcript segment with id {segment_id}"
        raise ValueError(msg)
    if old.superseded_by is not None:
        msg = f"turn #{segment_id} was already superseded - split the current version"
        raise ValueError(msg)

    new_ids: list[int] = []
    for frag in fragments:
        text = frag.text.strip()
        if not text:
            msg = "fragment text must not be blank"
            raise ValueError(msg)
        new_ids.append(
            store.add_transcript_segment(
                audio_segment_id=old.audio_segment_id,
                start=frag.start,
                end=frag.end,
                text=text,
                asr_model=HUMAN_MODEL,
                language=old.language,
                language_confidence=old.language_confidence,
                asr_confidence=HUMAN_CONFIDENCE,
                speaker_label=frag.speaker,
                provenance=f"split of #{old.id}",
                created=now,
            )
        )
        store.add_correction(
            transcript_segment_id=old.id,
            audio_segment_id=old.audio_segment_id,
            start=frag.start,
            end=frag.end,
            original_text=old.text,
            corrected_text=text,
            language=old.language,
            created=now,
            speaker=frag.speaker,
        )
    store.record_split(old.id, new_ids)
    return new_ids
