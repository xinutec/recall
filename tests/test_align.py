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


def test_handover_words_go_to_the_incoming_speaker() -> None:
    # The bug this rule exists for. SPEAKER_01 starts at 2.0 while SPEAKER_00 is still
    # finishing at 2.6, so the *exclusive* view awards the contested stretch to the
    # speaker already talking and its boundary sits late. Assigning by midpoint on that
    # view hands 01's first two words to 00; the overlap-aware view keeps both spans, so
    # coverage puts them where they belong.
    exclusive = [
        SpeakerTurn(speaker="SPEAKER_00", start=0.0, end=2.6),
        SpeakerTurn(speaker="SPEAKER_01", start=2.6, end=6.0),
    ]
    overlapping = [
        SpeakerTurn(speaker="SPEAKER_00", start=0.0, end=2.6),
        SpeakerTurn(speaker="SPEAKER_01", start=2.0, end=6.0),
    ]
    words = [
        _w(0.5, 1.0, " so"),
        _w(1.1, 1.8, " anyway"),
        _w(2.1, 2.4, " well"),  # 01's, but inside 00's exclusive span
        _w(2.4, 2.6, " I"),  # ditto
        _w(2.7, 3.2, " think"),
        _w(3.3, 3.9, " so"),
    ]
    assert [a.text for a in assign_words_to_speakers(words, exclusive)] == [
        "so anyway well I",
        "think so",
    ]
    aligned = assign_words_to_speakers(words, exclusive, overlapping=overlapping)
    assert [(a.speaker, a.text) for a in aligned] == [
        ("SPEAKER_00", "so anyway"),
        ("SPEAKER_01", "well I think so"),
    ]


def test_backchannel_leaves_the_continuing_speaker_alone() -> None:
    # The other shape of overlap: 01 says "mm-hm" over 00 mid-sentence. Both cover the
    # contested words completely, so coverage ties — and the tie-break (who is still
    # talking latest) keeps the words with 00, who is producing the speech around them.
    exclusive = [SpeakerTurn(speaker="SPEAKER_00", start=0.0, end=6.0)]
    overlapping = [
        SpeakerTurn(speaker="SPEAKER_00", start=0.0, end=6.0),
        SpeakerTurn(speaker="SPEAKER_01", start=2.4, end=2.9),
    ]
    words = [
        _w(0.5, 1.0, " we"),
        _w(2.5, 2.8, " should"),  # inside 01's backchannel
        _w(3.0, 3.6, " go"),
    ]
    aligned = assign_words_to_speakers(words, exclusive, overlapping=overlapping)
    assert [(a.speaker, a.text) for a in aligned] == [("SPEAKER_00", "we should go")]


def test_a_long_contested_stretch_is_not_treated_as_a_handover() -> None:
    # How the unbounded rule lost its measurement. Here pyannote has both speakers
    # active for a full 3s — cross-talk or an uncertain boundary, not a handover. The
    # incoming speaker covers every contested word and takes them all, moving the
    # boundary EARLY. A bound shorter than the contested stretch refuses that and keeps
    # the exclusive answer; a bound longer than it does not.
    exclusive = [
        SpeakerTurn(speaker="SPEAKER_00", start=0.0, end=4.0),
        SpeakerTurn(speaker="SPEAKER_01", start=4.0, end=8.0),
    ]
    overlapping = [
        SpeakerTurn(speaker="SPEAKER_00", start=0.0, end=4.0),
        SpeakerTurn(speaker="SPEAKER_01", start=1.0, end=8.0),  # 3s contested
    ]
    words = [
        _w(0.2, 0.8, " these"),
        _w(1.5, 2.0, " words"),  # inside the contested stretch, truly SPEAKER_00's
        _w(2.5, 3.0, " are"),
        _w(3.2, 3.8, " mine"),
        _w(4.5, 5.0, " yours"),
    ]
    unbounded = assign_words_to_speakers(words, exclusive, overlapping=overlapping)
    assert [(a.speaker, a.text) for a in unbounded] == [
        ("SPEAKER_00", "these"),
        ("SPEAKER_01", "words are mine yours"),  # the early-boundary failure
    ]
    bounded = assign_words_to_speakers(
        words, exclusive, overlapping=overlapping, overlap_bound_s=0.4
    )
    assert [(a.speaker, a.text) for a in bounded] == [
        ("SPEAKER_00", "these words are mine"),
        ("SPEAKER_01", "yours"),
    ]


def test_the_bound_still_lets_a_real_handover_through() -> None:
    # A genuine handover overlaps by a moment, so it stays under the bound and the
    # incoming speaker still gets their opening words — the bound must not neuter the
    # rule, only stop it running away.
    exclusive = [
        SpeakerTurn(speaker="SPEAKER_00", start=0.0, end=2.6),
        SpeakerTurn(speaker="SPEAKER_01", start=2.6, end=6.0),
    ]
    overlapping = [
        SpeakerTurn(speaker="SPEAKER_00", start=0.0, end=2.6),
        SpeakerTurn(speaker="SPEAKER_01", start=2.3, end=6.0),  # 0.3s contested
    ]
    words = [
        _w(0.5, 1.0, " so"),
        _w(1.1, 1.8, " anyway"),
        _w(2.35, 2.55, " well"),
        _w(2.7, 3.2, " I"),
        _w(3.3, 3.9, " think"),
    ]
    aligned = assign_words_to_speakers(
        words, exclusive, overlapping=overlapping, overlap_bound_s=0.4
    )
    assert [(a.speaker, a.text) for a in aligned] == [
        ("SPEAKER_00", "so anyway"),
        ("SPEAKER_01", "well I think"),
    ]


def test_overlap_aware_matches_midpoint_when_nothing_overlaps() -> None:
    # The two views coincide on old pyannote (and on any clip without simultaneous
    # speech), and then the coverage rule must reproduce the midpoint rule exactly —
    # otherwise shipping it would change untold turns for no reason.
    words = [
        _w(0.0, 0.5, " can"),
        _w(0.5, 1.0, " you"),
        _w(1.1, 1.3, " do"),  # straddles the 1.2 boundary, 40/60
        _w(1.6, 2.0, " 29"),
        _w(2.0, 2.5, " april"),
        _w(3.0, 3.5, " thanks"),
        _w(5.0, 5.4, " bye"),  # past every turn — the gap fallback
    ]
    assert assign_words_to_speakers(words, TURNS, overlapping=TURNS) == (
        assign_words_to_speakers(words, TURNS)
    )


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
