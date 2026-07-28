"""Ingest captured audio segments into the store as transcripts.

For each segment: derive the normalised working copy, transcribe it, map the
result to absolute-time drafts, and write them. The transcriber is injected, so
this orchestration is testable with a stub (no model required).
"""

from __future__ import annotations

from collections.abc import Iterable
from datetime import timedelta
from pathlib import Path

from recall.asr import (
    Transcriber,
    combine_result,
    make_working_copy,
    result_to_drafts,
    scratch_wav,
    slice_clip,
)
from recall.diarize import Diarizer
from recall.loops import is_repetition_loop
from recall.store import Store
from recall.timeline import Segment
from recall.vad import Vad, overlaps_speech


def ingest_transcripts(  # noqa: PLR0913 - pipeline collaborators + output config
    store: Store,
    segments: Iterable[Segment],
    transcriber: Transcriber,
    *,
    work_dir: Path,
    model_name: str,
    vad: Vad | None = None,
) -> int:
    """Transcribe `segments` and write their transcripts. Returns rows written.

    With a `vad`, only spans it flags as speech are transcribed (each on its own,
    like a turn) — silence is skipped, so Whisper can't hallucinate on it. Without
    one, the whole clip is transcribed (legacy path). Either way every segment is
    marked processed so a silent one isn't retried forever.
    """
    written = 0
    for segment in segments:
        audio_id = store.add_audio_segment(segment)
        # VAD first: a segment with no speech is skipped entirely — never run
        # Whisper on silence (it's wasteful and triggers the temperature-fallback
        # re-decode churn). Cheap, and it keeps the worker able to keep up.
        regions = vad(Path(segment.path)) if vad is not None else None
        if regions is not None and not regions:
            store.mark_transcribed(audio_id)
            continue
        # Transcribe the WHOLE segment so Whisper has full context (short isolated
        # clips make it loop / mis-detect language), then drop the turns that fall
        # in VAD silence — gating hallucinations without slicing tiny clips.
        with scratch_wav(work_dir / f"{Path(segment.path).stem}.wav") as working:
            make_working_copy(Path(segment.path), working)
            result = transcriber(working)  # working copy is scratch, dropped here
        drafts = result_to_drafts(
            result, segment_start=segment.start, model_name=model_name
        )
        for draft in drafts:
            if is_repetition_loop(draft.text):
                continue  # degenerate loop output -> drop at the source
            if regions is not None:
                rel_start = (draft.start - segment.start).total_seconds()
                rel_end = (draft.end - segment.start).total_seconds()
                if not overlaps_speech(rel_start, rel_end, regions):
                    continue  # turn sits in VAD silence -> hallucination, skip
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
            written += 1
        store.mark_transcribed(audio_id)
    return written


def ingest_diarized(  # noqa: PLR0913 - pipeline collaborators + output config
    store: Store,
    segments: Iterable[Segment],
    diarizer: Diarizer,
    transcriber: Transcriber,
    *,
    work_dir: Path,
    model_name: str,
) -> int:
    """Diarize each segment, then transcribe each speaker turn on its own.

    Per-turn transcription means each turn auto-detects its own language (the fix
    for whole-clip language lock) and is tagged with the diarizer's relative
    speaker label. Returns transcript rows written.
    """
    written = 0
    for segment in segments:
        audio_id = store.add_audio_segment(segment)
        stem = Path(segment.path).stem
        # Working copy and per-turn clips are scratch — deleted on exit so work/
        # never balloons (see scratch_wav).
        with scratch_wav(work_dir / f"{stem}.wav") as working:
            make_working_copy(Path(segment.path), working)
            for index, turn in enumerate(diarizer(working).exclusive):
                with scratch_wav(work_dir / f"{stem}-turn{index:04d}.wav") as clip:
                    slice_clip(working, clip, turn.start, turn.end)
                    result = transcriber(clip)
                text, confidence = combine_result(result)
                if not text or is_repetition_loop(text):
                    continue
                store.add_transcript_segment(
                    audio_segment_id=audio_id,
                    start=segment.start + timedelta(seconds=turn.start),
                    end=segment.start + timedelta(seconds=turn.end),
                    text=text,
                    asr_model=model_name,
                    language=result.language,
                    language_confidence=result.language_confidence,
                    asr_confidence=confidence,
                    speaker_cluster=turn.speaker,
                    provenance=model_name,
                )
                written += 1
        store.mark_transcribed(audio_id)
    return written
