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

from recall.cleanup import HALLUCINATION_REASON, LOOP_REASON
from recall.ids import AudioSegmentId, TranscriptId
from recall.store import Store

# A turn hidden for one of these was hidden because of what it *was*, not because
# something replaced it. Restoring it would undo a judgement, not a bug.
EVIDENCE_REASONS = (HALLUCINATION_REASON, LOOP_REASON)


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
    """Every segment a refine emptied: it once had turns, and now shows none."""
    blanked = []
    for audio_id, turns in store.segments_showing_no_turns().items():
        restore = last_generation(turns)
        if not restore:
            continue
        texts = [t[2] for t in turns if t[0] in restore]
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
