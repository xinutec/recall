//! The display rules, exercised through the crate's public surface.
//!
//! A search hit shows an unconfirmed speaker guess; a read-through transcript
//! must not assert it. The paired tests below pin both sides of that.
//!
//! The private helpers (`clarity`, `who_detail`) are tested through
//! [`cli::render::details`], which pins what a person reads.

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
        loudness: None,
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
        speaker: Some("Pippijn".to_owned()),
        speaker_confirmed: true,
        speaker_confidence: Some(0.4),
        ..turn()
    };
    assert_eq!(attribution(&t), "Pippijn");
    assert_eq!(who(&t), "Pippijn");
}

/// Search shows an unconfirmed guess with its strength; it is the only signal most hits have.
#[test]
fn a_search_hit_shows_an_unconfirmed_guess_with_its_strength() {
    let t = Turn {
        speaker: Some("Pippijn".to_owned()),
        speaker_confidence: Some(0.764),
        ..turn()
    };
    assert_eq!(attribution(&t), "Pippijn ~76%");
}

/// A read-through transcript must not assert the same unconfirmed name.
#[test]
fn a_transcript_never_asserts_an_unconfirmed_guess() {
    let t = Turn {
        speaker: Some("Pippijn".to_owned()),
        speaker_confidence: Some(0.764),
        ..turn()
    };
    assert_eq!(who(&t), "SPEAKER_01");
}

#[test]
fn a_guess_with_no_score_gets_no_percentage() {
    let t = Turn {
        speaker: Some("Pippijn".to_owned()),
        ..turn()
    };
    assert_eq!(attribution(&t), "Pippijn");
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

/// A `SPEAKER_*` in the label column is diarization's placeholder, not a person,
/// and the diagnostic dump must not report it as a confirmed name.
#[test]
fn a_confirmed_speaker_placeholder_is_not_reported_as_confirmed() {
    let t = Turn {
        speaker: Some("SPEAKER_02".to_owned()),
        speaker_confirmed: true,
        ..turn()
    };
    let out = details(&[7], std::slice::from_ref(&t));
    assert!(out.contains("who      : unknown"), "got:\n{out}");
}

#[test]
fn the_audibility_bands_are_the_labelling_uis() {
    for (loudness, band) in [
        (None, "unmeasured"),
        (Some(0.05), "clear"),
        (Some(0.01), "quiet"),
        (Some(0.009), "faint"),
    ] {
        let t = Turn { loudness, ..turn() };
        let out = details(&[7], std::slice::from_ref(&t));
        assert!(
            out.contains(&format!("({band})")),
            "{loudness:?} → {band}:\n{out}"
        );
    }
}

/// `/api/transcripts` answers with the current turn, so a dump asked about a
/// superseded id must say so rather than print new text under the old number.
#[test]
fn a_dump_says_when_the_id_that_answered_is_not_the_one_asked_for() {
    let out = details(&[3], &[turn()]);
    assert!(out.contains("#3 was superseded by this"), "got:\n{out}");
}

#[test]
fn a_dump_of_the_id_that_was_asked_for_is_plainly_visible() {
    let out = details(&[7], &[turn()]);
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
