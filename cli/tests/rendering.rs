//! The display rules. The private helpers are tested through
//! [`cli::render::details`].

use cli::api::Turn;
use cli::render::{attribution, details, hit, transcript, who};

fn turn() -> Turn {
    Turn {
        id: 7,
        start: "2026-09-10T12:00:00+00:00".to_owned(),
        end: "2026-09-10T12:00:04+00:00".to_owned(),
        text: "hello".to_owned(),
        language: Some("en".to_owned()),
        speaker: None,
        speaker_confirmed: false,
        speaker_confidence: None,
        confidence: Some(0.5),
        model: Some("whisper".to_owned()),
        tier: "transcribed".to_owned(),
        hidden: None,
        source: Some("usb".to_owned()),
        cluster: Some("SPEAKER_01".to_owned()),
    }
}

#[test]
fn a_confirmed_name_wins_and_carries_no_score() {
    let t = Turn {
        speaker: Some("Oskar".to_owned()),
        speaker_confirmed: true,
        speaker_confidence: Some(0.4),
        ..turn()
    };
    assert_eq!(attribution(&t), "Oskar");
    assert_eq!(who(&t), "Oskar");
}

#[test]
fn a_search_hit_shows_an_unconfirmed_guess_with_its_strength() {
    let t = Turn {
        speaker: Some("Oskar".to_owned()),
        speaker_confidence: Some(0.764),
        ..turn()
    };
    assert_eq!(attribution(&t), "Oskar ~76%");
}

#[test]
fn a_transcript_never_asserts_an_unconfirmed_guess() {
    let t = Turn {
        speaker: Some("Oskar".to_owned()),
        speaker_confidence: Some(0.764),
        ..turn()
    };
    assert_eq!(who(&t), "SPEAKER_01");
}

#[test]
fn a_guess_with_no_score_gets_no_percentage() {
    let t = Turn {
        speaker: Some("Oskar".to_owned()),
        ..turn()
    };
    assert_eq!(attribution(&t), "Oskar");
}

#[test]
fn with_neither_a_name_nor_a_voice_it_is_unknown() {
    let t = Turn {
        cluster: None,
        ..turn()
    };
    assert_eq!(attribution(&t), "unknown");
    assert_eq!(who(&t), "unknown");
}

#[test]
fn a_dump_of_the_id_that_was_asked_for_is_plainly_visible() {
    let out = details(&[turn()]);
    assert!(out.contains("status   : visible"), "got:\n{out}");
}

#[test]
fn a_search_line_carries_the_language_and_the_source() {
    let line = hit(&turn());
    assert!(line.contains("[en]"), "got: {line}");
    assert!(line.contains("(usb)"), "got: {line}");
    assert!(line.contains("hello"), "got: {line}");
}

#[test]
fn an_empty_transcript_still_has_its_heading() {
    assert_eq!(transcript("meeting-1", &[]), "# meeting-1\n");
}
