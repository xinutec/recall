//! Word-to-speaker alignment: the heart of the transcribe-then-assign pipeline.
//!
//! ⚠ These are `tests/test_align.py`'s cases, case for case. `recall.align` is
//! still live, so the two implementations must agree: a case that passes here
//! and not there is a divergence, not a Rust variant.

use recalld::align::{AlignedTurn, MIN_TURN_S, SpeakerTurn, Word, assign_words_to_speakers};

fn w(start: f64, end: f64, text: &str) -> Word {
    Word {
        start,
        end,
        text: text.to_owned(),
        probability: 0.9,
    }
}

fn turn(speaker: &str, start: f64, end: f64) -> SpeakerTurn {
    SpeakerTurn {
        speaker: speaker.to_owned(),
        start,
        end,
    }
}

fn turns() -> Vec<SpeakerTurn> {
    vec![
        turn("SPEAKER_00", 0.0, 1.2),
        turn("SPEAKER_01", 1.2, 2.8),
        turn("SPEAKER_00", 2.8, 4.0),
    ]
}

fn said(aligned: &[AlignedTurn]) -> Vec<(&str, &str)> {
    aligned
        .iter()
        .map(|a| (a.speaker.as_str(), a.text.as_str()))
        .collect()
}

#[test]
fn groups_consecutive_words_by_speaker() {
    let words = [
        w(0.0, 0.5, " can"),
        w(0.5, 1.0, " you"),
        w(1.6, 2.0, " 29"),
        w(2.0, 2.5, " april"),
        w(3.0, 3.5, " thanks"),
    ];
    let aligned = assign_words_to_speakers(&words, &turns(), MIN_TURN_S);
    assert_eq!(
        said(&aligned),
        vec![
            ("SPEAKER_00", "can you"),
            ("SPEAKER_01", "29 april"),
            ("SPEAKER_00", "thanks"),
        ]
    );
    // The turn's span is its first/last WORD, not a diarization boundary.
    assert!((aligned[1].start - 1.6).abs() < f64::EPSILON);
    assert!((aligned[1].end - 2.5).abs() < f64::EPSILON);
}

#[test]
fn a_word_in_a_gap_goes_to_the_nearest_turn() {
    // A word at 5.0s, past the last turn (ends 4.0) → nearest is final SPEAKER_00.
    let aligned = assign_words_to_speakers(&[w(5.0, 5.4, " bye")], &turns(), MIN_TURN_S);
    assert_eq!(said(&aligned), vec![("SPEAKER_00", "bye")]);
}

#[test]
fn empty_inputs_align_to_nothing() {
    assert!(assign_words_to_speakers(&[], &turns(), MIN_TURN_S).is_empty());
    assert!(assign_words_to_speakers(&[w(0.0, 0.5, " hi")], &[], MIN_TURN_S).is_empty());
}

#[test]
fn smooths_a_single_jitter_flipped_word() {
    // One speaker talks continuously, but a brief diarization blip plus
    // word-timestamp jitter lands one word in the other speaker's span. It must
    // be absorbed, not split into its own one-word turn (the ping-pong bug).
    let blipped = vec![
        turn("SPEAKER_00", 0.0, 2.0),
        turn("SPEAKER_01", 2.0, 2.3), // 0.3s blip
        turn("SPEAKER_00", 2.3, 5.0),
    ];
    let words = [
        w(0.0, 0.5, " we"),
        w(0.6, 1.1, " are"),
        w(2.0, 2.3, " going"), // midpoint lands in the SPEAKER_01 blip
        w(2.4, 2.9, " to"),
        w(3.0, 3.5, " release"),
    ];
    let aligned = assign_words_to_speakers(&words, &blipped, MIN_TURN_S);
    assert_eq!(
        said(&aligned),
        vec![("SPEAKER_00", "we are going to release")]
    );
}

#[test]
fn an_aligned_turn_carries_its_words() {
    // Per-word timings ride along — the basis for audio-exact edits later.
    let words = [
        w(0.0, 0.5, " can"),
        w(0.5, 1.0, " you"),
        w(1.6, 2.0, " 29"),
        w(2.0, 2.5, " april"),
    ];
    let aligned = assign_words_to_speakers(&words, &turns(), MIN_TURN_S);
    let first: Vec<&str> = aligned[0].words.iter().map(|w| w.text.as_str()).collect();
    assert_eq!(first, vec![" can", " you"]);
    assert!((aligned[0].words[0].start - 0.0).abs() < f64::EPSILON);
    assert!((aligned[0].words[1].end - 1.0).abs() < f64::EPSILON);
    let second: Vec<&str> = aligned[1].words.iter().map(|w| w.text.as_str()).collect();
    assert_eq!(second, vec![" 29", " april"]);
}

#[test]
fn keeps_genuine_turns_above_the_threshold() {
    // A real exchange (each turn well over the threshold) is preserved, not merged.
    let words = [
        w(0.0, 0.5, " can"),
        w(0.5, 1.0, " you"),
        w(1.6, 2.0, " 29"),
        w(2.0, 2.5, " april"),
    ];
    let aligned = assign_words_to_speakers(&words, &turns(), MIN_TURN_S);
    assert_eq!(
        said(&aligned),
        vec![("SPEAKER_00", "can you"), ("SPEAKER_01", "29 april")]
    );
}

#[test]
fn confidence_is_the_mean_word_probability() {
    let words = [
        Word {
            start: 0.0,
            end: 0.5,
            text: " a".to_owned(),
            probability: 0.6,
        },
        Word {
            start: 0.5,
            end: 1.0,
            text: " b".to_owned(),
            probability: 1.0,
        },
    ];
    let aligned = assign_words_to_speakers(&words, &turns(), MIN_TURN_S);
    assert!((aligned[0].confidence - 0.8).abs() < 1e-9);
}

#[test]
fn an_exactly_tied_neighbour_pair_absorbs_the_left_one() {
    // The one branch neither the six cases above nor the 400 random ones reach:
    // a sub-threshold run whose neighbours are EXACTLY equal in duration. Random
    // float durations never tie, so the `>=` in `smooth` was unprotected —
    // flipping it to `>` left every other test green. Python resolves the tie to
    // the LEFT neighbour, and this pins that.
    let turns = vec![
        turn("A", 0.0, 1.0),
        turn("B", 1.0, 1.25),
        turn("C", 1.25, 3.0),
    ];
    let words = [
        w(0.0, 0.5, " one"),
        w(0.5, 1.0, " two"),
        w(1.0, 1.25, " hm"), // 0.25s: below the threshold, neighbours both 1.0s
        w(1.25, 1.75, " three"),
        w(1.75, 2.25, " four"),
    ];
    let aligned = assign_words_to_speakers(&words, &turns, MIN_TURN_S);
    assert_eq!(
        said(&aligned),
        vec![("A", "one two hm"), ("C", "three four")],
        "the tie must go to the left neighbour, as recall.align does"
    );
}
