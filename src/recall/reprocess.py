"""Re-transcribe the archive with an improved model — the "re-derive" pillar.

When the model improves (a LoRA fine-tune, a better base, more enrollment), this
re-runs transcription over existing machine-authored segments and supersedes them
with the new version. Outputs are versioned, never destroyed; **human corrections
are never touched** (the store excludes them).

The transcriber is injected, so this is testable with a stub "better" model.
"""

from __future__ import annotations

from pathlib import Path

from recall.asr import Transcriber, combine_result, scratch_wav, slice_clip
from recall.store import Store


def reprocess(
    store: Store,
    transcriber: Transcriber,
    *,
    work_dir: Path,
    model_name: str,
    max_confidence: float | None = None,
) -> int:
    """Re-transcribe eligible segments and supersede them. Returns count redone.

    `max_confidence` limits reprocessing to segments below that confidence (the
    cheap, high-value subset); omit it to reprocess everything machine-authored.
    """
    segments = store.reprocessable_segments(max_confidence=max_confidence)
    if not segments:
        return 0

    work_dir.mkdir(parents=True, exist_ok=True)
    redone = 0
    for segment in segments:
        if segment.audio_segment_id is None:
            continue
        ref = store.audio_segment_ref(segment.audio_segment_id)
        if ref is None:
            continue
        path, audio_start = ref
        rel_start = max(0.0, (segment.start - audio_start).total_seconds())
        rel_end = (segment.end - audio_start).total_seconds()
        with scratch_wav(work_dir / f"re-{segment.id:06d}.wav") as clip:
            slice_clip(Path(path), clip, rel_start, rel_end)
            result = transcriber(clip)  # clip is scratch, dropped on exit
        text, confidence = combine_result(result)
        if not text:
            continue

        # Don't degrade: keep the existing version if the new one is less
        # confident. (A real safety net — re-transcribing short clips can
        # hallucinate; the human review loop catches the rest.)
        if (
            segment.asr_confidence is not None
            and confidence is not None
            and confidence < segment.asr_confidence
        ):
            continue

        new_id = store.add_transcript_segment(
            audio_segment_id=segment.audio_segment_id,
            start=segment.start,
            end=segment.end,
            text=text,
            asr_model=model_name,
            language=result.language,
            language_confidence=result.language_confidence,
            asr_confidence=confidence,
            speaker_label=segment.speaker_label,
            speaker_id=segment.speaker_id,
        )
        store.supersede(segment.id, new_id)
        redone += 1
    return redone
