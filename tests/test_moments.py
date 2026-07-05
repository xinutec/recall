"""Cluster a conversation's turns into moments — same-time turns from different
mics folded into one, the best source as the spine, the rest as alternates."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import UTC, datetime, timedelta

from recall.moments import best_colocated_guess, cluster_moments

BASE = datetime(2026, 6, 21, 18, 0, 0, tzinfo=UTC)


@dataclass(frozen=True)
class T:
    """A minimal sourced turn (structurally a moments.SourcedTurn / GuessTurn)."""

    source_id: str
    s: float
    e: float
    asr_confidence: float | None
    tag: str = ""
    id: int = 0
    speaker_guess: str | None = None
    speaker_score: float | None = None

    @property
    def start(self) -> datetime:
        return BASE + timedelta(seconds=self.s)

    @property
    def end(self) -> datetime:
        return BASE + timedelta(seconds=self.e)


def test_same_time_two_mics_fold_into_one_moment() -> None:
    # Same utterance heard by two mics: one moment, the cleaner one as primary.
    usb = T("usb", 0.0, 30.0, 0.40, "noisy")
    pix = T("pixel9", 0.2, 29.0, 0.91, "clean")
    moments = cluster_moments([usb, pix])
    assert len(moments) == 1
    m = moments[0]
    assert m.primary == (pix,)  # highest confidence wins the spine
    assert m.alternates == (usb,)
    assert m.sources == ("pixel9", "usb")  # primary source first


def test_sequential_turns_stay_separate() -> None:
    # Non-overlapping turns are different moments, not folded.
    a = T("usb", 0.0, 5.0, 0.9)
    b = T("usb", 10.0, 15.0, 0.9)
    moments = cluster_moments([a, b])
    assert [m.primary for m in moments] == [(a,), (b,)]
    assert all(m.alternates == () for m in moments)


def test_single_source_is_one_turn_per_moment() -> None:
    # With one mic, nothing folds — backward compatible with the flat timeline.
    turns = [T("usb", 0.0, 4.0, 0.8), T("usb", 4.0, 8.0, 0.8)]
    moments = cluster_moments(turns)
    assert len(moments) == 2
    assert all(len(m.primary) == 1 and m.alternates == () for m in moments)


def test_best_source_split_is_preserved_as_spine() -> None:
    # pixel9 (clean) splits the moment into two speakers; usb merged it into one.
    # The clean source is the spine, so BOTH speakers survive in primary.
    p1 = T("pixel9", 0.0, 5.0, 0.9, "speakerA")
    p2 = T("pixel9", 5.0, 10.0, 0.9, "speakerB")
    merged = T("usb", 0.0, 10.0, 0.3, "merged")
    moments = cluster_moments([p1, merged, p2])
    assert len(moments) == 1
    m = moments[0]
    assert m.primary == (p1, p2)  # both speakers kept, in order
    assert m.alternates == (merged,)
    assert m.sources == ("pixel9", "usb")


def test_moment_span_comes_from_primary() -> None:
    usb = T("usb", 0.0, 30.0, 0.4)
    pix = T("pixel9", 0.2, 29.0, 0.9)
    m = cluster_moments([usb, pix])[0]
    assert m.start == pix.start and m.end == pix.end


def test_primary_borrows_a_stronger_colocated_guess() -> None:
    # Spine = cleaner *transcription* (usb), but the co-located mic heard the same
    # speech with a more confident voiceprint match — surface that on the spine turn.
    spine = T("usb", 0.0, 3.0, 0.9, id=1, speaker_guess="Pippijn", speaker_score=0.40)
    alt = T("pixel9", 0.5, 2.8, 0.5, id=2, speaker_guess="Pippijn", speaker_score=0.80)
    assert best_colocated_guess([spine], [alt]) == {1: ("Pippijn", 0.80)}


def test_primary_keeps_its_own_when_colocated_guess_is_weaker() -> None:
    spine = T("usb", 0.0, 3.0, 0.9, id=1, speaker_guess="Pippijn", speaker_score=0.80)
    alt = T("pixel9", 0.5, 2.8, 0.5, id=2, speaker_guess="Alice", speaker_score=0.30)
    assert best_colocated_guess([spine], [alt]) == {1: ("Pippijn", 0.80)}


def test_non_overlapping_alternate_is_not_borrowed() -> None:
    # A different moment in time isn't the same speech — don't borrow its guess.
    spine = T("usb", 0.0, 3.0, 0.9, id=1, speaker_guess="Pippijn", speaker_score=0.40)
    alt = T("pixel9", 10.0, 12.0, 0.5, id=2, speaker_guess="Pippijn", speaker_score=0.9)
    assert best_colocated_guess([spine], [alt]) == {1: ("Pippijn", 0.40)}


def test_primary_with_no_guess_takes_a_colocated_one() -> None:
    spine = T("usb", 0.0, 3.0, 0.9, id=1)  # no guess of its own
    alt = T("pixel9", 0.5, 2.8, 0.5, id=2, speaker_guess="Pippijn", speaker_score=0.70)
    assert best_colocated_guess([spine], [alt]) == {1: ("Pippijn", 0.70)}


def test_existing_name_is_not_flipped_to_a_different_one() -> None:
    # Identity-preserving: a stronger but DIFFERENT name from a time-overlap doesn't
    # flip the spine's person (phone-clock skew makes overlap an unreliable signal) —
    # only same-name mics raise confidence.
    spine = T("usb", 0.0, 3.0, 0.9, id=1, speaker_guess="Pippijn", speaker_score=0.40)
    alt = T("pixel9", 0.5, 2.8, 0.5, id=2, speaker_guess="Alice", speaker_score=0.90)
    assert best_colocated_guess([spine], [alt]) == {1: ("Pippijn", 0.40)}
