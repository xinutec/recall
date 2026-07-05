"""Fold a conversation's turns into moments.

Every source (USB mic, each phone) transcribes the same wall-clock moment
independently, so one utterance produces a parallel turn per mic. This groups
those duplicates into a single *moment*: the highest-confidence source becomes
the spine (its turns are `primary`, so a multi-speaker split it caught survives),
and the other sources' overlapping turns ride along as `alternates` — the
corroborating versions the UI folds behind a "compare" affordance.

Pure and decoupled, like recall.conversations: it operates on anything exposing a
turn's span, source, and confidence (store.TranscriptSegment satisfies it once it
carries source_id), so it's testable without the store.
"""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import dataclass
from datetime import datetime
from typing import Protocol


class SourcedTurn(Protocol):
    """The slice of a turn moment-folding needs (read-only, so a frozen
    dataclass satisfies it structurally)."""

    @property
    def start(self) -> datetime: ...
    @property
    def end(self) -> datetime: ...
    @property
    def source_id(self) -> str | None: ...
    @property
    def asr_confidence(self) -> float | None: ...


class GuessTurn(Protocol):
    """The slice needed to borrow the best co-located voiceprint guess: a turn's id,
    its span, and its guess + match strength."""

    @property
    def id(self) -> int: ...
    @property
    def start(self) -> datetime: ...
    @property
    def end(self) -> datetime: ...
    @property
    def speaker_guess(self) -> str | None: ...
    @property
    def speaker_score(self) -> float | None: ...


def best_colocated_guess(
    primary: Sequence[GuessTurn], alternates: Sequence[GuessTurn]
) -> dict[int, tuple[str | None, float | None]]:
    """For each primary (spine) turn, the most-confident speaker guess among it and the
    time-overlapping alternates — the same speech caught by other mics.

    The spine is chosen for the cleanest *transcription* (`_to_moment`), which says
    nothing about *attribution*: a co-located mic may carry a stronger voiceprint match
    for the very same words. So the displayed guess is refined from the overlapping
    versions, while the text stays the spine's. Returns {primary id: (guess, score)};
    a turn with nothing to add keeps its own. Confirmed human labels are unaffected —
    this only refines the auto guess.

    Identity-preserving on purpose: a guess's confidence is raised only by mics that
    agree on the *person*, and a missing guess is filled from the most-confident
    overlapping version — but an existing name is never *flipped* on a time-overlap
    alone. The phone-clock skew (arrival-stamped, lagging a variable few seconds) makes
    raw time-overlap an unreliable "same speaker" signal, so borrowing a different name
    from it could assert the wrong person; corroborating the same name only strengthens.
    """
    chosen: dict[int, tuple[str | None, float | None]] = {}
    for turn in primary:
        overlapping = [
            alt
            for alt in alternates
            if alt.speaker_guess is not None
            and alt.start < turn.end
            and alt.end > turn.start
        ]
        guess, score = turn.speaker_guess, turn.speaker_score
        if guess is None:
            # No guess of our own: fill from the most-confident co-located version.
            best = max(
                overlapping,
                key=lambda a: a.speaker_score if a.speaker_score is not None else -1.0,
                default=None,
            )
            if best is not None:
                guess, score = best.speaker_guess, best.speaker_score
        else:
            # Have a guess: raise its confidence only from mics that name the same
            # person — never flip the identity (see the skew note above).
            for alt in overlapping:
                if (
                    alt.speaker_guess == guess
                    and alt.speaker_score is not None
                    and (score is None or alt.speaker_score > score)
                ):
                    score = alt.speaker_score
        chosen[turn.id] = (guess, score)
    return chosen


@dataclass(frozen=True)
class Moment[T: SourcedTurn]:
    """One wall-clock moment. `primary` is the best source's turn(s) — its
    segmentation kept, so a multi-speaker split survives — and `alternates` are
    the other sources' overlapping turns, kept for the compare view."""

    primary: tuple[T, ...]
    alternates: tuple[T, ...]

    @property
    def start(self) -> datetime:
        return min(turn.start for turn in self.primary)

    @property
    def end(self) -> datetime:
        return max(turn.end for turn in self.primary)

    @property
    def sources(self) -> tuple[str, ...]:
        """Distinct sources that captured this moment, primary's source first."""
        seen: list[str] = []
        for turn in (*self.primary, *self.alternates):
            name = turn.source_id
            if name and name not in seen:
                seen.append(name)
        return tuple(seen)


def cluster_moments[T: SourcedTurn](turns: Sequence[T]) -> list[Moment[T]]:
    """Fold chronologically-ordered turns into moments.

    `turns` must be sorted ascending by start (store.recent_transcripts is). The
    same merge-overlapping-intervals sweep conversations uses groups every turn
    that overlaps the cluster's running span — so the same utterance heard by
    several mics lands in one moment while sequential utterances stay separate.
    """
    clusters: list[list[T]] = []
    current: list[T] = []
    running_end: datetime | None = None
    for turn in turns:
        if running_end is not None and turn.start < running_end:
            current.append(turn)
            running_end = max(running_end, turn.end)
        else:
            if current:
                clusters.append(current)
            current = [turn]
            running_end = turn.end
    if current:
        clusters.append(current)
    return [_to_moment(cluster) for cluster in clusters]


def _confidence(turns: Sequence[SourcedTurn]) -> float:
    return sum((turn.asr_confidence or 0.0) for turn in turns)


def _to_moment[T: SourcedTurn](cluster: list[T]) -> Moment[T]:
    by_source: dict[str | None, list[T]] = {}
    for turn in cluster:
        by_source.setdefault(turn.source_id, []).append(turn)
    # Spine = the source with the highest summed confidence (cleaner audio scores
    # higher); ties go to the one with more turns, i.e. the finer speaker split.
    best = max(
        by_source,
        key=lambda source: (_confidence(by_source[source]), len(by_source[source])),
    )
    primary = sorted(by_source[best], key=lambda turn: turn.start)
    alternates = sorted(
        (
            turn
            for source, turns in by_source.items()
            if source != best
            for turn in turns
        ),
        key=lambda turn: turn.start,
    )
    return Moment(primary=tuple(primary), alternates=tuple(alternates))
