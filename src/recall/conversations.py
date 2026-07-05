"""Group a continuous transcript stream into conversations.

Capture is always-on, so a "conversation" is a maximal run of turns with no
silence longer than a gap threshold between them. This v1 uses the inter-turn
gap only (turn start/end). It deliberately does *not* split on participant
changes — speaker coverage is still partial — and leaves silence-grounded (VAD)
detection to a later pass. The gap is the one knob, exposed for calibration.

Pure and decoupled: it segments any sequence of objects that expose a turn's
span and speaker (recall.store.TranscriptSegment satisfies it structurally), so
it's testable without the store.
"""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import dataclass
from datetime import datetime
from typing import Protocol

# A conversation breaks after a silence longer than this between consecutive
# turns. Five minutes is a starting point — calibrate against hand-labelled days.
DEFAULT_GAP_SECONDS = 300.0


class Turn(Protocol):
    """The slice of a transcript turn segmentation needs.

    Read-only members (properties), so a frozen dataclass like
    store.TranscriptSegment satisfies it structurally.
    """

    @property
    def id(self) -> int: ...
    @property
    def start(self) -> datetime: ...
    @property
    def end(self) -> datetime: ...
    @property
    def speaker_label(self) -> str | None: ...


@dataclass(frozen=True)
class Conversation[T: Turn]:
    """A contiguous run of turns, keeping the turns themselves for the caller."""

    turns: tuple[T, ...]

    @property
    def start(self) -> datetime:
        return self.turns[0].start

    @property
    def end(self) -> datetime:
        return max(turn.end for turn in self.turns)

    @property
    def turn_count(self) -> int:
        return len(self.turns)

    @property
    def speakers(self) -> tuple[str, ...]:
        """Distinct known speakers, in first-seen order (unknown turns omitted)."""
        seen: list[str] = []
        for turn in self.turns:
            name = turn.speaker_label
            if name and name not in seen:
                seen.append(name)
        return tuple(seen)


def segment_conversations[T: Turn](
    turns: Sequence[T], *, gap_seconds: float = DEFAULT_GAP_SECONDS
) -> list[Conversation[T]]:
    """Split chronologically-ordered turns into conversations on silence gaps.

    `turns` must be sorted ascending by start and already filtered to current,
    non-hidden turns (store.recent_transcripts does both). A break is inserted
    wherever the silence before a turn — measured from the running maximum end so
    far, so overlapping turns don't manufacture a gap — is strictly greater than
    `gap_seconds`.
    """
    conversations: list[Conversation[T]] = []
    current: list[T] = []
    prev_end: datetime | None = None
    for turn in turns:
        if prev_end is not None:
            gap = (turn.start - prev_end).total_seconds()
            if gap > gap_seconds:
                conversations.append(Conversation(tuple(current)))
                current = []
                prev_end = None
        current.append(turn)
        prev_end = turn.end if prev_end is None else max(prev_end, turn.end)
    if current:
        conversations.append(Conversation(tuple(current)))
    return conversations
