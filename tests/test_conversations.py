"""Grouping a continuous transcript stream into conversations (pure)."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import UTC, datetime, timedelta

from recall.conversations import DEFAULT_GAP_SECONDS, segment_conversations

BASE = datetime(2026, 6, 13, 12, 0, 0, tzinfo=UTC)


@dataclass(frozen=True)
class FakeTurn:
    id: int
    start: datetime
    end: datetime
    speaker_label: str | None = None


def _turn(tid: int, at: float, dur: float, speaker: str | None = None) -> FakeTurn:
    return FakeTurn(
        id=tid,
        start=BASE + timedelta(seconds=at),
        end=BASE + timedelta(seconds=at + dur),
        speaker_label=speaker,
    )


def test_empty_stream_has_no_conversations() -> None:
    assert segment_conversations([]) == []


def test_a_tight_back_and_forth_is_one_conversation() -> None:
    turns = [_turn(1, 0, 2), _turn(2, 3, 2), _turn(3, 7, 2)]
    convs = segment_conversations(turns, gap_seconds=60)
    assert len(convs) == 1
    assert convs[0].turn_count == 3
    assert [t.id for t in convs[0].turns] == [1, 2, 3]


def test_a_silence_longer_than_the_gap_splits_the_conversation() -> None:
    # 2s turns at 0 and 5 (gap 3s), then one at 400 (gap ~393s) -> two groups.
    turns = [_turn(1, 0, 2), _turn(2, 5, 2), _turn(3, 400, 2)]
    convs = segment_conversations(turns, gap_seconds=60)
    assert [[t.id for t in c.turns] for c in convs] == [[1, 2], [3]]


def test_gap_exactly_at_the_threshold_does_not_split() -> None:
    # end at 2, next start at 62 -> gap is exactly 60s; only a strictly larger
    # silence breaks, so this stays one conversation.
    turns = [_turn(1, 0, 2), _turn(2, 62, 2)]
    assert len(segment_conversations(turns, gap_seconds=60)) == 1
    # one second more of silence does split it
    later = [_turn(1, 0, 2), _turn(2, 63, 2)]
    assert len(segment_conversations(later, gap_seconds=60)) == 2


def test_threshold_is_tunable() -> None:
    turns = [_turn(1, 0, 2), _turn(2, 100, 2)]
    assert len(segment_conversations(turns, gap_seconds=60)) == 2
    assert len(segment_conversations(turns, gap_seconds=200)) == 1


def test_overlapping_turns_stay_together() -> None:
    # A long turn [0,120] overlapped by a short one starting at 5 — the running
    # max end (120) means the gap to a turn at 130 is 10s, not 125s.
    turns = [_turn(1, 0, 120), _turn(2, 5, 3), _turn(3, 130, 2)]
    convs = segment_conversations(turns, gap_seconds=60)
    assert len(convs) == 1
    assert convs[0].end == BASE + timedelta(seconds=130 + 2)


def test_conversation_summary_fields() -> None:
    turns = [
        _turn(1, 0, 2, "Carol"),
        _turn(2, 3, 2, "Alice"),
        _turn(3, 7, 2, "Carol"),
        _turn(4, 10, 2, None),  # unknown speaker excluded from participants
    ]
    (conv,) = segment_conversations(turns, gap_seconds=60)
    assert conv.start == BASE
    assert conv.end == BASE + timedelta(seconds=12)
    assert conv.turn_count == 4
    # distinct known speakers, in first-seen order, no None
    assert conv.speakers == ("Carol", "Alice")


def test_default_gap_is_a_sane_few_minutes() -> None:
    assert 60 <= DEFAULT_GAP_SECONDS <= 900


def test_default_gap_splits_a_just_over_five_minute_silence() -> None:
    # With no gap_seconds, a 300.5s silence (> 300, < 301) splits on the default —
    # pins DEFAULT_GAP_SECONDS at exactly 300, not merely "a few minutes".
    turns = [_turn(1, 0, 2), _turn(2, 302.5, 2)]  # gap = 302.5 - 2 = 300.5s
    assert len(segment_conversations(turns)) == 2


def test_conversation_is_frozen_hence_hashable() -> None:
    # @dataclass(frozen=True) makes a Conversation hashable; frozen=False would set
    # __hash__ to None and hashing would raise — so this pins the frozen contract.
    conv = segment_conversations([_turn(1, 0, 2)])[0]
    assert isinstance(hash(conv), int)
