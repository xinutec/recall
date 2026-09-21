//! What may be brought back when a refine emptied a segment, and what must not.

// ⓘ The text rules themselves are tested in audiocore, where they live.
use doctor::blanked::{HiddenTurn, any_restorable, last_generation};

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
