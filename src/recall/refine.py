"""Refine the archive with diarization — split a segment by speaker, keeping context.

The basic worker transcribes each audio segment as one block (VAD splits on silence,
not speakers), so a fast back-and-forth lands in one merged turn with one (often
wrong) speaker. This re-derives a segment the high-quality way: transcribe the WHOLE
segment once — full context, so the language is detected reliably and Whisper's
anti-hallucination decoding works — then diarize the same audio and assign each word
to whoever was speaking (recall.align). The result is the full-context transcription
split by speaker at word granularity, each turn embedded and matched to the enrolled
voiceprints on the spot. Human corrections are kept; nothing is deleted.

Heavy (pyannote + word-level Whisper per segment), so it runs only while capture is
idle (see `recall refine`). The collaborators are injected, so this is testable.
"""

from __future__ import annotations

import sys
from datetime import datetime, timedelta
from pathlib import Path

from recall.align import AlignedTurn, assign_words_to_speakers
from recall.asr import (
    AsrResult,
    Transcriber,
    Word,
    make_working_copy,
    scratch_wav,
    slice_clip,
)
from recall.diarize import Diarizer
from recall.identify import rematch_speaker_guesses
from recall.ids import AudioSegmentId
from recall.loops import is_repetition_loop
from recall.speakerid import Embedder
from recall.store import (
    ALIGNED_MARKER,
    DIARIZED_MARKER,
    HOUSEHOLD_LANGUAGES,
    Correction,
    Store,
)


def _hits_human(start: datetime, end: datetime, human: list[Correction]) -> bool:
    """True if [start, end) overlaps any human-corrected span (absolute times)."""
    return any(c.start < end and c.end > start for c in human)


# A refined pass that comes back with far less text than the segment already has is a
# degenerate transcription (a truncated long-form decode, a whole-clip mis-detection),
# not a real improvement — and swapping it in would hide the good transcript from every
# view. Below this fraction of the existing visible text we keep what's there and skip
# the swap. Only enforced once there's a substantial transcript to protect; tiny
# segments swing too wildly in ratio for the bar to mean anything.
_MIN_COVERAGE_RATIO = 0.5
_COVERAGE_REF_MIN_CHARS = 200


def _replace_turns(  # noqa: PLR0913 - the whole per-segment write context
    store: Store,
    *,
    audio_id: AudioSegmentId,
    segment_start: datetime,
    aligned: list[AlignedTurn],
    result: AsrResult,
    human: list[Correction],
    model_name: str,
) -> list[tuple[int, int, AlignedTurn]] | None:
    """Atomically swap a segment's old machine turns for the aligned replacements.

    Called only now — with the replacements computed and about to be written.
    Hiding the old turns earlier (before the slow transcribe + diarize) blanks the
    segment for minutes, so a reader mid-pass sees an empty recording. Human turns
    are never touched; the 'diarized' marker keeps the segment from being re-picked
    — which is also why the hide and the inserts are ONE transaction: a crash
    between them would otherwise leave the segment blank and never re-picked.

    Returns (index, turn_id, turn) for each turn written, for the embed pass — or
    `None` if the coverage guard refused the swap (the pass produced far less text
    than the segment already had), leaving the existing transcript untouched.
    """
    # A non-household language for the whole segment is the model hallucinating on
    # unclear audio — keep the turns (with their audio) but flag them unreliable.
    reliable = result.language in HOUSEHOLD_LANGUAGES
    written: list[tuple[int, int, AlignedTurn]] = []
    with store.transaction():
        existing = list(store.visible_machine_turns_for_audio(audio_id))
        # Guard: refuse to replace a substantial transcript with a far smaller one.
        # That's a degenerate pass, and hiding the good turns would blank the recording.
        existing_chars = sum(len(o.text) for o in existing)
        new_chars = sum(len(t.text) for t in aligned)
        if (
            existing_chars >= _COVERAGE_REF_MIN_CHARS
            and new_chars < _MIN_COVERAGE_RATIO * existing_chars
        ):
            return None
        for old in existing:
            store.hide(old.id, f"{DIARIZED_MARKER} ({model_name})")
        for index, turn in enumerate(aligned):
            if is_repetition_loop(turn.text):
                continue
            start = segment_start + timedelta(seconds=turn.start)
            end = segment_start + timedelta(seconds=turn.end)
            if _hits_human(start, end, human):
                continue  # ground truth already covers this span
            turn_id = store.add_transcript_segment(
                audio_segment_id=audio_id,
                start=start,
                end=end,
                text=turn.text,
                asr_model=model_name,
                language=result.language,
                language_confidence=result.language_confidence,
                asr_confidence=turn.confidence if reliable else 0.0,
                speaker_cluster=turn.speaker,
                provenance=f"{ALIGNED_MARKER} ({model_name})",
                # Word timings re-based to this turn's start, so a later boundary
                # edit can snap to a real word time and play exactly that span.
                word_timings=[
                    Word(
                        start=w.start - turn.start,
                        end=w.end - turn.start,
                        text=w.text,
                        probability=w.probability,
                    )
                    for w in turn.words
                ],
            )
            written.append((index, turn_id, turn))
    return written


def refine_diarized(  # noqa: PLR0913 - pipeline collaborators + output config
    store: Store,
    diarizer: Diarizer,
    transcriber: Transcriber,
    embedder: Embedder,
    *,
    work_dir: Path,
    model_name: str,
    limit: int = 100_000,
    redo: bool = False,
    source: str | None = None,
    audio_ids: list[AudioSegmentId] | None = None,
) -> int:
    """Diarize-refine up to `limit` segments. Returns turns added.

    Per segment: transcribe the whole thing once with word timings (full context),
    diarize it, then assign words to speakers — so each stored turn has reliable
    language and text, split by speaker, and is attributed to a person on the spot.

    `audio_ids` refines exactly those segments (e.g. an on-demand request for one
    stretch). Else `source` forces a full re-derive of one recording — every one of its
    segments, regardless of state — the clean way to rebuild a recording through the
    canonical pipeline. Otherwise `redo=False` picks segments never diarized and
    `redo=True` upgrades ones from an older pipeline. Either way it's resumable.
    """
    work_dir.mkdir(parents=True, exist_ok=True)
    added = 0
    embedded = 0
    skipped = 0
    if audio_ids is not None:
        pass  # caller chose the exact segments
    elif source is not None:
        audio_ids = store.audio_segments_for_source(source, limit=limit)
    elif redo:
        audio_ids = store.audio_segments_to_rediarize(limit=limit)
    else:
        audio_ids = store.audio_segments_to_diarize(limit=limit)
    for audio_id in audio_ids:
        segment = store.audio_segment(audio_id)
        if segment is None or not Path(segment.path).exists():
            continue
        human = store.human_corrections_overlapping(
            audio_id, segment.start, segment.end
        )

        stem = Path(segment.path).stem
        # The working copy and every per-turn clip are scratch — sliced, consumed
        # (transcribe/diarize/embed), then deleted on exit so work/ never balloons.
        with scratch_wav(work_dir / f"{stem}.wav") as working:
            make_working_copy(Path(segment.path), working)

            # Transcribe the whole segment once (full context, word timings), diarize
            # it, and assign each word to the active speaker.
            result = transcriber(working)
            speakers = list(diarizer(working))
            aligned = assign_words_to_speakers(list(result.words), speakers)

            written = _replace_turns(
                store,
                audio_id=audio_id,
                segment_start=segment.start,
                aligned=aligned,
                result=result,
                human=human,
                model_name=model_name,
            )
            if written is None:
                # Coverage guard tripped: kept the existing transcript, wrote nothing.
                # Left un-diarized on purpose, so a later (fixed) pass can re-derive it.
                skipped += 1
                continue
            added += len(written)
            # Embed each turn's audio + match to enrolled voiceprints (immediate
            # attribution) — after the atomic swap, so the slow per-clip work never
            # holds the write lock; a bad clip must never block the turn or the pass.
            for index, turn_id, turn in written:
                with scratch_wav(work_dir / f"{stem}-turn{index:04d}.wav") as clip:
                    try:
                        # slice_clip is inside the guard too: a corrupt-frame source
                        # makes ffmpeg fail on some turns (e.g. an mp3 with a missing
                        # header), and that must skip the turn's embedding — never crash
                        # the pass (which would leave the request re-picked forever).
                        slice_clip(working, clip, turn.start, turn.end)
                        store.set_embedding(turn_id, embedder(clip))
                        embedded += 1
                    except Exception:  # a bad clip never blocks the turn or the pass
                        continue
    if skipped:
        print(
            f"refine: kept the existing transcript on {skipped} segment(s) — the "
            "refined pass covered too little to trust (coverage guard)",
            file=sys.stderr,
        )
    if embedded:
        rematch_speaker_guesses(store)
    return added
