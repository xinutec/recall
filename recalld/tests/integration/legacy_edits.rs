//! Every shape of human data found on 2026-10-04 becomes the acts it means,
//! and a row that means nothing says why.

use recalld::legacy_edits::{Correction, Hide, NamedLine, Origin, convert};
use transcript::{Act, ClipId, Instant, Language, SourceId, Span};

fn at(s: i64) -> Instant {
    Instant::from_micros(1_759_000_000_000_000 + s * 1_000_000).unwrap()
}

fn span(a: i64, b: i64) -> Span {
    Span::new(at(a), at(b)).unwrap()
}

fn correction(
    id: i64,
    original: &str,
    corrected: &str,
    speaker: Option<&str>,
    checked: bool,
) -> Correction {
    Correction {
        id,
        at: at(100 + id),
        clip: ClipId::from_stored(7),
        span: span(0, 4),
        original: original.into(),
        corrected: corrected.into(),
        speaker: speaker.map(Into::into),
        checked,
        hidden_reason: None,
    }
}

fn kinds(acts: &[(Instant, Act, Origin)]) -> Vec<&'static str> {
    acts.iter()
        .map(|(_, act, _)| match act {
            Act::Words { .. } => "words",
            Act::Speaker { .. } => "speaker",
            Act::NoSpeech { .. } => "no-speech",
            Act::Unintelligible { .. } => "unintelligible",
            Act::Voice { .. } => "voice",
            Act::Language { .. } => "language",
            Act::Retract { .. } => "retract",
        })
        .collect()
}

#[test]
fn naming_a_line_is_a_speaker_act_only() {
    let out = convert(
        &[correction(1, "hallo", "hallo", Some("Alex"), false)],
        &[],
        &[],
        &[],
    );
    assert_eq!(kinds(&out.acts), ["speaker"]);
    assert!(
        matches!(&out.acts[0].1, Act::Speaker { name, enrol: true, .. } if name.as_str() == "Alex")
    );
}

#[test]
fn fixing_words_and_naming_is_both_and_the_words_are_vouched_for() {
    let out = convert(
        &[correction(
            1,
            "hallo daar",
            "hallo Sam",
            Some("Alex"),
            false,
        )],
        &[],
        &[],
        &[],
    );
    assert_eq!(kinds(&out.acts), ["words", "speaker"]);
    assert!(
        matches!(&out.acts[0].1, Act::Words { text, checked: true, .. } if text.as_str() == "hallo Sam")
    );
}

#[test]
fn words_are_right_is_a_checked_words_act_with_the_same_words() {
    let out = convert(
        &[correction(1, "goed zo", "goed zo", None, true)],
        &[],
        &[],
        &[],
    );
    assert_eq!(kinds(&out.acts), ["words"]);
}

#[test]
fn a_clip_judged_unusable_for_a_voiceprint_keeps_its_name_but_not_enrolment() {
    let mut c = correction(1, "x", "x", Some("Robin"), false);
    c.hidden_reason = Some("not a household voice".into());
    let out = convert(&[c], &[], &[], &[]);
    assert!(matches!(&out.acts[0].1, Act::Speaker { enrol: false, .. }));
}

#[test]
fn a_correction_that_did_nothing_says_so() {
    let out = convert(
        &[correction(1, "zelfde", "zelfde", None, false)],
        &[],
        &[],
        &[],
    );
    assert!(out.acts.is_empty());
    assert_eq!(
        out.none,
        [(
            Origin::Correction(1),
            "same words, no speaker, not checked: nothing done"
        )]
    );
}

#[test]
fn a_piece_of_a_split_corrected_line_carries_words_and_a_name() {
    let piece = NamedLine {
        id: 9,
        at: at(200),
        clip: ClipId::from_stored(7),
        span: span(2, 4),
        text: "tweede deel".into(),
        speaker: Some("Sam".into()),
        human_text: true,
    };
    let named = NamedLine {
        human_text: false,
        text: "model words".into(),
        id: 10,
        ..piece.clone()
    };
    let out = convert(&[], &[piece, named], &[], &[]);
    assert_eq!(kinds(&out.acts), ["words", "speaker", "speaker"]);
}

#[test]
fn hides_and_pins_become_their_acts_and_an_unknown_language_says_why() {
    let hide = |id, unintelligible| Hide {
        id,
        at: at(300),
        clip: ClipId::from_stored(7),
        span: span(0, 1),
        unintelligible,
    };
    let pins = [
        (
            SourceId::parse("meeting-20261003-1108").unwrap(),
            Some(Language::Dutch),
        ),
        (SourceId::parse("meeting-20261003-1029").unwrap(), None),
    ];
    let out = convert(&[], &[], &[hide(1, false), hide(2, true)], &pins);
    let mut got = kinds(&out.acts);
    got.sort_unstable();
    assert_eq!(got, ["language", "no-speech", "unintelligible"]);
    assert_eq!(out.none.len(), 1);
}

#[test]
fn acts_come_out_in_the_order_they_were_done() {
    let early = correction(1, "a", "b", None, false);
    let late = correction(5, "c", "d", None, false);
    let out = convert(&[late, early], &[], &[], &[]);
    assert!(out.acts[0].0 < out.acts[1].0);
}

#[test]
fn a_vouched_blank_correction_means_nobody_spoke() {
    // How the Check page recorded "nobody spoke" over Whisper's invented
    // "Thank you." on 26-28 September.
    let out = convert(
        &[correction(1, "Thank you.", "", None, true)],
        &[],
        &[],
        &[],
    );
    assert_eq!(kinds(&out.acts), ["no-speech"]);
    assert!(out.none.is_empty());
}
