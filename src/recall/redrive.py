"""Re-derive the archive's transcripts with the current (fixed) pipeline.

Re-transcribes each audio segment full-context + VAD-filtered (the loop-free
path), then:
- soft-hides the segment's old machine turns (reason "reprocessed …") — kept,
  recoverable, never deleted;
- adds the new turns, but **skips any that overlap a human correction** so
  ground truth is never clobbered (re-attach-by-audio-time).

Human turns themselves are never touched. The transcriber/VAD are injected, so
this is testable with stubs.
"""

from __future__ import annotations

from datetime import datetime
from pathlib import Path

from recall.asr import Transcriber, make_working_copy, result_to_drafts, scratch_wav
from recall.quality import is_repetition_loop
from recall.store import REPROCESSED_MARKER, Correction, Store
from recall.vad import Vad, overlaps_speech


def _hits_human(start: datetime, end: datetime, human: list[Correction]) -> bool:
    """True if [start, end) overlaps any human-corrected span (absolute times)."""
    return any(c.start < end and c.end > start for c in human)


def redrive_archive(  # noqa: PLR0913 - pipeline collaborators + output config
    store: Store,
    transcriber: Transcriber,
    vad: Vad,
    *,
    work_dir: Path,
    model_name: str,
    limit: int = 100_000,
) -> int:
    """Re-transcribe up to `limit` not-yet-redriven segments. Returns rows added.

    Resumable: only segments without a 'reprocessed' marker are picked, so small
    chunks can be run repeatedly (e.g. during quiet periods) until none remain —
    keeping load off the live capture.
    """
    work_dir.mkdir(parents=True, exist_ok=True)
    added = 0
    for audio_id in store.audio_segments_to_redrive(limit=limit):
        segment = store.audio_segment(audio_id)
        if segment is None or not Path(segment.path).exists():
            continue
        regions = vad(Path(segment.path))
        human = store.human_corrections_overlapping(
            audio_id, segment.start, segment.end
        )

        if not regions:
            # No speech: the old machine turns were hallucinations — hide, add nothing.
            with store.transaction():
                for old in store.visible_machine_turns_for_audio(audio_id):
                    store.hide(old.id, f"{REPROCESSED_MARKER} ({model_name})")
            continue

        with scratch_wav(work_dir / f"{Path(segment.path).stem}.wav") as working:
            make_working_copy(Path(segment.path), working)
            result = transcriber(working)  # working copy is scratch, dropped here
        drafts = result_to_drafts(
            result, segment_start=segment.start, model_name=model_name
        )

        # Only now — with the replacements computed and about to be written — supersede
        # the old machine turns. Hiding them before the slow transcribe blanks the
        # segment if that transcribe fails mid-pass (and to any reader mid-pass), so a
        # crash leaves an empty recording. Human turns are never touched. Mirrors
        # refine_diarized, which orders it the same way for the same reason — and,
        # like it, makes the hide + inserts ONE transaction: a crash between them
        # would leave the segment blank and (marker present) never re-picked.
        with store.transaction():
            for old in store.visible_machine_turns_for_audio(audio_id):
                store.hide(old.id, f"{REPROCESSED_MARKER} ({model_name})")

            for draft in drafts:
                if is_repetition_loop(draft.text):
                    continue  # degenerate loop output -> drop
                rel_start = (draft.start - segment.start).total_seconds()
                rel_end = (draft.end - segment.start).total_seconds()
                if not overlaps_speech(rel_start, rel_end, regions):
                    continue
                if _hits_human(draft.start, draft.end, human):
                    continue  # ground truth already covers this span
                store.add_transcript_segment(
                    audio_segment_id=audio_id,
                    start=draft.start,
                    end=draft.end,
                    text=draft.text,
                    asr_model=draft.asr_model,
                    language=draft.language,
                    language_confidence=draft.language_confidence,
                    asr_confidence=draft.asr_confidence,
                    provenance=model_name,
                )
                added += 1
    return added
