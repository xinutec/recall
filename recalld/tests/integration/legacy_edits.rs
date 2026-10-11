//! Every shape of human data found on 2026-10-04 becomes the acts it means,
//! and a row that means nothing says why.

use recalld::legacy_edits::{Correction, Hide, HideKind, NamedLine, Origin, Recovered};
use transcript::{Act, ClipId, Instant, Language, SourceId, Span};

/// The conversion, with "now" well after every act in these tests.
fn convert(
    corrections: &[Correction],
    lines: &[NamedLine],
    hides: &[Hide],
    pins: &[(SourceId, Option<Language>)],
) -> Recovered {
    recalld::legacy_edits::convert(corrections, lines, hides, pins, at(10_000))
}

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
        over: span(0, 4),
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
            Act::Exclude { .. } => "exclude",
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
        over: span(2, 4),
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
    let hide = |id, kind| Hide {
        id,
        at: at(300),
        clip: ClipId::from_stored(7),
        span: span(0, 1),
        kind,
    };
    let pins = [
        (
            SourceId::parse("meeting-20261003-1108").unwrap(),
            Some(Language::Dutch),
        ),
        (SourceId::parse("meeting-20261003-1029").unwrap(), None),
    ];
    let out = convert(
        &[],
        &[],
        &[
            hide(1, HideKind::NoSpeech),
            hide(2, HideKind::Unintelligible),
            hide(3, HideKind::Exclude),
        ],
        &pins,
    );
    let mut got = kinds(&out.acts);
    got.sort_unstable();
    assert_eq!(got, ["exclude", "language", "no-speech", "unintelligible"]);
    assert_eq!(out.none.len(), 1);
}

#[test]
fn each_hide_a_person_decided_is_its_act_and_a_passs_own_hide_is_none() {
    // The bulk hides were each approved with their count: silence is "nobody
    // spoke", test speech and a second copy of a minute are left out. A
    // pass's replacement of its own lines is re-derived by render.
    for (reason, kind) in [
        ("nobody spoke", Some(HideKind::NoSpeech)),
        ("silent minute", Some(HideKind::NoSpeech)),
        ("near-silent minute", Some(HideKind::NoSpeech)),
        ("no speech in span", Some(HideKind::NoSpeech)),
        ("can't make out (human)", Some(HideKind::Unintelligible)),
        ("test speech played into the house", Some(HideKind::Exclude)),
        ("a second copy of the same minute", Some(HideKind::Exclude)),
        ("diarized (per-mic runner)", None),
        ("set aside for re-transcription", None),
        ("split into pieces (12)", None),
    ] {
        assert_eq!(HideKind::of(reason), kind, "{reason}");
    }
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

#[test]
fn a_recording_that_shows_nothing_today_is_hidden_whole_once_and_one_that_shows_a_line_by_line() {
    let dir = tempfile::tempdir().expect("tmp");
    let meaning = recalld::work::open_write(dir.path()).expect("meaning");
    recalld::meaning_schema::ensure(&meaning).expect("schema");
    let ingest = recalld::store::open(dir.path()).expect("ingest");
    for name in ["usb-20261004T120000.flac", "usb-20261004T120100.flac"] {
        recalld::store::insert(
            &ingest,
            &recalld::store::Row {
                source: "usb".into(),
                filename: name.into(),
                start_utc: format!("2026-10-04T12:0{}:00Z", &name[16..17]),
                bytes: 1,
                sha256: "x".into(),
                received_utc: "2026-10-04T12:05:00Z".into(),
                sent_utc: None,
            },
        )
        .expect("segment");
    }
    let path = |name: &str| dir.path().join("ingest/usb").join(name);
    meaning
        .execute_batch(&format!(
            "INSERT INTO sources (id, name, kind) VALUES ('usb', 'usb', 'coreaudio');
             INSERT INTO audio_segments (id, source_id, path, start_utc, end_utc, sample_rate, channels) VALUES
               (1, 'usb', '{}', '2026-10-04T12:00:00+00:00', '2026-10-04T12:01:00+00:00', 48000, 1),
               (2, 'usb', '{}', '2026-10-04T12:01:00+00:00', '2026-10-04T12:02:00+00:00', 48000, 1);
             INSERT INTO transcript_segments (audio_segment_id, start_utc, end_utc, text, asr_model, hidden_reason, created_utc) VALUES
               (1, '2026-10-04T12:00:10+00:00', '2026-10-04T12:00:12+00:00', 'a', 'whisper', 'silent minute', '2026-10-04T12:03:00+00:00'),
               (1, '2026-10-04T12:00:20+00:00', '2026-10-04T12:00:22+00:00', 'b', 'whisper', 'silent minute', '2026-10-04T12:03:00+00:00'),
               (2, '2026-10-04T12:01:10+00:00', '2026-10-04T12:01:12+00:00', 'c', 'whisper', 'near-silent minute', '2026-10-04T12:03:00+00:00'),
               (2, '2026-10-04T12:01:20+00:00', '2026-10-04T12:01:22+00:00', 'kept', 'whisper', NULL, '2026-10-04T12:03:00+00:00');",
            path("usb-20261004T120000.flac").display(),
            path("usb-20261004T120100.flac").display(),
        ))
        .expect("rows");

    let loaded = recalld::legacy_edits::load(&meaning, &ingest, dir.path()).expect("load");
    let spans: Vec<(i64, i64)> = loaded
        .hides
        .iter()
        .map(|h| {
            let clip_start = Instant::parse("2026-10-04T12:00:00Z").unwrap().micros();
            (
                (h.span.start().micros() - clip_start) / 1_000_000,
                (h.span.end().micros() - clip_start) / 1_000_000,
            )
        })
        .collect();
    assert_eq!(spans, [(0, 60), (70, 72)], "whole and once; line by line");
}
