"""Bring back transcripts a refine pass blanked.

`refine._replace_turns` hid a segment's turns and then wrote their replacements — but
the filters that drop a turn (a repetition loop, a span a human had already corrected)
ran *after* the hide. A pass whose every turn was filtered out, or which produced none
at all, hid the transcript and put nothing in its place. The segment went blank.

It emptied 175 segments of real conversation that way, among them a minute of Dutch
about writing things down in order to remember them — which is, precisely, what this
archive is for. The refine no longer does it (it replaces a transcript or keeps it,
never empties one), but the segments it already emptied are still empty.

They are recoverable, because a hide is soft: every generation of turns is still in the
table, and the newest is the best transcript that pass ever produced. This restores it.

The rule is narrow on purpose. Only a segment showing *nothing at all* is touched — if
any turn still stands, the pipeline's current view is right and is left alone. And only
turns hidden for *provenance* (superseded by a later pass) come back; one hidden because
the VAD heard no speech, or because it was a repetition loop, was hidden on the evidence
and stays hidden.
"""

from __future__ import annotations

from dataclasses import dataclass

from recall.cleanup import (
    CLEANUP_HIDE_REASONS,
    HALLUCINATION_REASON,
    is_wordless,
)
from recall.ids import AudioSegmentId, TranscriptId
from recall.quality import is_repetition_loop
from recall.store import Store

# A turn hidden for one of these was hidden because of what it *was*, not because
# something replaced it. Restoring it would undo a judgement, not a bug.
# Derived, never restated: `cleanup` owns what it hides for, and every one of those
# is evidence about the turn rather than provenance. Restating the list here is how
# scan-wordless and scan-foreign-script briefly became restorable.
EVIDENCE_REASONS = CLEANUP_HIDE_REASONS


def restorable_texts(texts: list[str]) -> list[str]:
    """The subset worth bringing back: whatever cleanup would not hide on sight.

    Pure text only — the foreign-script rule needs audio, and `find_blanked` has
    already excluded segments the detector heard nothing in, so script over real
    speech is protected there rather than here.
    """
    return [t for t in texts if not is_wordless(t) and not is_repetition_loop(t)]


@dataclass(frozen=True)
class Blanked:
    """A segment left with no visible turn: a pass hid what it did not replace."""

    audio_id: AudioSegmentId
    restore: tuple[TranscriptId, ...]
    preview: str


def last_generation(
    turns: list[tuple[TranscriptId, str, str]],
) -> tuple[TranscriptId, ...]:
    """The newest generation: the trailing run, in id order, hidden by a single pass.

    Each pass hides the generation before it, so a segment accretes them — original,
    reprocessed, diarized — and the *last* run sharing one hidden reason is the newest,
    and best, transcript that ever existed for it. Pure, so the rule is testable.

    `turns` is (id, hidden_reason, text) in id order. A turn hidden on the evidence of
    what it was (a hallucination, a repetition loop) is never restored.
    """
    generations = [t for t in turns if t[1] not in EVIDENCE_REASONS]
    if not generations:
        return ()
    newest_reason = generations[-1][1]
    run: list[TranscriptId] = []
    for turn_id, reason, _text in reversed(generations):
        if reason != newest_reason:
            break
        run.append(turn_id)
    return tuple(reversed(run))


def find_blanked(store: Store) -> list[Blanked]:
    """Every segment a refine emptied *that actually held speech*.

    The detector gates this. Not every provenance hide was a bug: sometimes a later
    pass was correctly dropping a hallucination, and restoring that resurrects garbage.
    On this archive 12 of 170 restorations did exactly that — "E aí", "т т т т",
    repeated glyphs on -64 dB silence — and they then blocked the cleanup by making an
    empty minute look transcribed. A segment the VAD heard nothing in gets nothing back.
    """
    silent = store.segments_with_no_detected_speech()
    blanked = []
    for audio_id, turns in store.segments_showing_no_turns().items():
        if audio_id in silent:
            continue  # the detector heard nothing here; there is no transcript to save
        restore = last_generation(turns)
        if not restore:
            continue
        texts = [t[2] for t in turns if t[0] in restore]
        if not restorable_texts(texts):
            continue  # every generation here is junk; restoring it re-makes work
        blanked.append(
            Blanked(
                audio_id=audio_id,
                restore=restore,
                preview=" | ".join(texts)[:80],
            )
        )
    return blanked


def restore(store: Store, blanked: list[Blanked]) -> int:
    """Unhide the newest generation on each blanked segment. Returns turns restored."""
    restored = 0
    for segment in blanked:
        for turn_id in segment.restore:
            store.unhide(int(turn_id))
            restored += 1
    return restored


def retract_into_silence(store: Store) -> list[tuple[TranscriptId, str]]:
    """Hide every machine turn standing on audio the detector heard nothing in.

    Whisper hallucinates on silence — "Thank you.", "音楽", "E aí", a glyph repeated
    forty times — and such a turn is not evidence of speech but of an empty room. It
    also
    *blocks the cleanup*: the transcript veto sees a turn and protects a minute that
    holds nothing, so dead air stays on the disk to defend a hallucination.

    The detector is the authority here as everywhere else: it listened to the audio, and
    the audio is the fact. Human turns are never touched — a person's judgement outranks
    a
    model's, in both directions.
    """
    hidden = []
    for turn_id, text in store.machine_turns_on_silent_audio():
        store.hide(int(turn_id), HALLUCINATION_REASON)
        hidden.append((turn_id, text))
    return hidden
