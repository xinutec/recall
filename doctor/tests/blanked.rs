//! What may be brought back when a refine emptied a segment, and what must not.

use doctor::blanked::{
    HiddenTurn, any_restorable, is_repetition_loop, is_wordless, last_generation,
};

#[test]
fn punctuation_only_turns_are_wordless() {
    assert!(is_wordless("..."));
    assert!(is_wordless(" *** "));
    assert!(is_wordless("!"));
    assert!(is_wordless("\u{2026}"));
    assert!(is_wordless(""));
    assert!(!is_wordless("Ja."));
    // Dutch is half this archive: an accented word is a word.
    assert!(!is_wordless("héél"));
}

#[test]
fn a_long_word_repeated_three_times_is_a_loop_and_a_short_one_is_emphasis() {
    assert!(is_repetition_loop("everything everything everything"));
    // Real speech. Hiding this would delete somebody actually saying it.
    assert!(!is_repetition_loop("no no no"));
    assert!(!is_repetition_loop("who who who"));
}

#[test]
fn a_dominant_token_over_enough_words_is_a_loop() {
    assert!(is_repetition_loop(
        "momentum a momentum b momentum c momentum"
    ));
}

#[test]
fn a_repeated_phrase_is_a_loop() {
    assert!(is_repetition_loop(
        "see you on the phone see you on the phone see you on the phone"
    ));
}

#[test]
fn a_spaceless_unit_repeated_four_times_is_a_loop() {
    assert!(is_repetition_loop("ASTASTASTAST"));
    assert!(is_repetition_loop("obaobaobaoba"));
    // Three repeats of a 2-char unit is only 6 characters — under the floor,
    // and the floor is what keeps "haha" and "bye bye" out of it.
    assert!(!is_repetition_loop("hahaha"));
}

#[test]
fn ordinary_speech_is_not_a_loop() {
    assert!(!is_repetition_loop("Ik denk dat we morgen gaan."));
    assert!(!is_repetition_loop("Shall we have dinner at seven?"));
    assert!(!is_repetition_loop(""));
}

fn turn(id: i64, reason: &str, text: &str) -> HiddenTurn {
    HiddenTurn {
        id,
        hidden_reason: reason.to_owned(),
        text: text.to_owned(),
    }
}

#[test]
fn the_newest_generation_is_the_trailing_run_of_one_reason() {
    let turns = [
        turn(1, "reprocessed", "first"),
        turn(2, "reprocessed", "first again"),
        turn(3, "diarized", "second"),
        turn(4, "diarized", "second again"),
    ];
    assert_eq!(last_generation(&turns), vec![3, 4]);
}

#[test]
fn a_turn_hidden_on_evidence_is_never_restored() {
    let turns = [
        turn(1, "reprocessed", "real speech"),
        turn(2, "no speech detected (VAD)", "Thank you."),
    ];
    // The hallucination is skipped entirely, so the newest RESTORABLE
    // generation is the one before it.
    assert_eq!(last_generation(&turns), vec![1]);

    let only_evidence = [turn(1, "repetition loop", "ASTASTASTAST")];
    assert!(last_generation(&only_evidence).is_empty());
}

#[test]
fn a_generation_of_pure_junk_is_not_worth_restoring() {
    assert!(!any_restorable(&["...", "ASTASTASTAST"]));
    assert!(any_restorable(&["...", "Ja, dat doen we."]));
}
