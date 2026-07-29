"""Word-to-speaker alignment: the heart of the transcribe-then-assign pipeline."""

from __future__ import annotations

from recall.align import assign_words_to_speakers
from recall.asr import Word
from recall.diarize import SpeakerTurn


def _w(start: float, end: float, text: str) -> Word:
    return Word(start=start, end=end, text=text, probability=0.9)


TURNS = [
    SpeakerTurn(speaker="SPEAKER_00", start=0.0, end=1.2),
    SpeakerTurn(speaker="SPEAKER_01", start=1.2, end=2.8),
    SpeakerTurn(speaker="SPEAKER_00", start=2.8, end=4.0),
]


def test_groups_consecutive_words_by_speaker() -> None:
    words = [
        _w(0.0, 0.5, " can"),
        _w(0.5, 1.0, " you"),
        _w(1.6, 2.0, " 29"),
        _w(2.0, 2.5, " april"),
        _w(3.0, 3.5, " thanks"),
    ]
    aligned = assign_words_to_speakers(words, TURNS)
    assert [(a.speaker, a.text) for a in aligned] == [
        ("SPEAKER_00", "can you"),
        ("SPEAKER_01", "29 april"),
        ("SPEAKER_00", "thanks"),
    ]
    # the turn's span is its first/last word, not a diarization boundary
    assert (aligned[1].start, aligned[1].end) == (1.6, 2.5)


def test_word_in_a_gap_goes_to_the_nearest_turn() -> None:
    # a word at 5.0s, past the last turn (ends 4.0) → nearest is the final SPEAKER_00
    aligned = assign_words_to_speakers([_w(5.0, 5.4, " bye")], TURNS)
    assert [(a.speaker, a.text) for a in aligned] == [("SPEAKER_00", "bye")]


def test_empty_inputs() -> None:
    assert assign_words_to_speakers([], TURNS) == []
    assert assign_words_to_speakers([_w(0.0, 0.5, " hi")], []) == []


def test_smooths_a_single_jitter_flipped_word() -> None:
    # One speaker talks continuously, but a brief diarization blip + word-timestamp
    # jitter lands one word in the other speaker's span. It must be absorbed, not
    # split into its own one-word turn (the ping-pong bug).
    turns = [
        SpeakerTurn(speaker="SPEAKER_00", start=0.0, end=2.0),
        SpeakerTurn(speaker="SPEAKER_01", start=2.0, end=2.3),  # 0.3s blip
        SpeakerTurn(speaker="SPEAKER_00", start=2.3, end=5.0),
    ]
    words = [
        _w(0.0, 0.5, " we"),
        _w(0.6, 1.1, " are"),
        _w(2.0, 2.3, " going"),  # midpoint lands in the SPEAKER_01 blip
        _w(2.4, 2.9, " to"),
        _w(3.0, 3.5, " release"),
    ]
    aligned = assign_words_to_speakers(words, turns)
    assert [a.speaker for a in aligned] == ["SPEAKER_00"]
    assert aligned[0].text == "we are going to release"


def test_aligned_turn_carries_its_words() -> None:
    # Per-word timings ride along on the turn — the basis for audio-exact edits later.
    aligned = assign_words_to_speakers(
        [
            _w(0.0, 0.5, " can"),
            _w(0.5, 1.0, " you"),
            _w(1.6, 2.0, " 29"),
            _w(2.0, 2.5, " april"),
        ],
        TURNS,
    )
    assert [w.text for w in aligned[0].words] == [" can", " you"]
    assert (aligned[0].words[0].start, aligned[0].words[1].end) == (0.0, 1.0)
    assert [w.text for w in aligned[1].words] == [" 29", " april"]


def test_keeps_genuine_turns_above_the_threshold() -> None:
    # A real exchange (each turn well over the threshold) is preserved, not merged.
    aligned = assign_words_to_speakers(
        [
            _w(0.0, 0.5, " can"),
            _w(0.5, 1.0, " you"),
            _w(1.6, 2.0, " 29"),
            _w(2.0, 2.5, " april"),
        ],
        TURNS,
    )
    assert [(a.speaker, a.text) for a in aligned] == [
        ("SPEAKER_00", "can you"),
        ("SPEAKER_01", "29 april"),
    ]
