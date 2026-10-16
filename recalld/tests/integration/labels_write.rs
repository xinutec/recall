//! The labelling writes append what a person did to the edit log and the view
//! shows it in the reply: words, names, nobody spoke, labelled fragments, span
//! assignment, voices and languages, each with its refusals and its undo.

use crate::fixture::Fleet;
use recalld::labels_write::{self as w, Correction, WriteError};
use recalld::line::LineId;
use transcript::{EditId, Language, SourceId};

/// One usb clip: two sentences, two voices.
fn fleet() -> Fleet {
    let f = Fleet::new();
    f.clip(
        "usb",
        "coreaudio",
        "20261004T120000",
        &[(1.0, 3.0, "goede morgen."), (10.0, 12.0, "hoe gaat het?")],
        Some(&[("SPEAKER_00", 0.0, 5.0), ("SPEAKER_01", 5.0, 60.0)]),
    );
    f.refresh();
    f
}

fn line(f: &Fleet, text: &str) -> LineId {
    LineId::from_stored(
        f.shown()
            .into_iter()
            .find(|l| l.text == text)
            .unwrap_or_else(|| panic!("no line {text:?} in {:?}", f.texts()))
            .id,
    )
}

#[test]
fn typed_words_show_at_once_on_the_same_line_and_the_models_are_kept_hidden() {
    let f = fleet();
    let id = line(&f, "hoe gaat het?");

    let shown = w::correct(
        &f.meaning,
        &f.ingest,
        f.root(),
        id,
        "hoe gaat het met je?",
        &Correction::default(),
    )
    .expect("correct");

    assert_eq!(shown, id, "the line keeps its id");
    let l = f
        .shown()
        .into_iter()
        .find(|l| l.id == id.stored())
        .expect("line");
    assert_eq!(
        (l.text.as_str(), l.tier.as_str(), l.model.as_str()),
        ("hoe gaat het met je?", "corrected", "human")
    );
    assert_eq!(f.hidden()[0].1, "hoe gaat het?");
    assert_eq!(f.act_kinds(), ["Words"]);
}

#[test]
fn naming_a_speaker_is_a_speaker_act_only_and_a_check_is_a_checked_words_act() {
    let f = fleet();
    let id = line(&f, "goede morgen.");

    w::correct(
        &f.meaning,
        &f.ingest,
        f.root(),
        id,
        "goede morgen.",
        &Correction {
            speaker: Some("Robin"),
            words_checked: false,
        },
    )
    .expect("name");
    assert_eq!(f.act_kinds(), ["Speaker"]);
    let named = f
        .shown()
        .into_iter()
        .find(|l| l.id == id.stored())
        .expect("line");
    assert_eq!(named.label.as_deref(), Some("Robin"));
    assert_eq!(
        named.model,
        recalld::view::SHIM_MODEL,
        "the words are still the model's"
    );

    w::correct(
        &f.meaning,
        &f.ingest,
        f.root(),
        id,
        "goede morgen.",
        &Correction {
            speaker: None,
            words_checked: true,
        },
    )
    .expect("check");
    assert_eq!(f.act_kinds(), ["Speaker", "Words"]);
    let checked: i64 = f
        .meaning
        .query_row(
            "SELECT words_checked FROM rendered_lines WHERE id = ?1",
            [id.stored()],
            |r| r.get(0),
        )
        .expect("row");
    assert_eq!(checked, 1);
}

#[test]
fn a_write_names_what_is_wrong_with_it() {
    let f = fleet();
    let id = line(&f, "goede morgen.");
    let correct = |id, text: &str| {
        w::correct(
            &f.meaning,
            &f.ingest,
            f.root(),
            id,
            text,
            &Correction::default(),
        )
    };
    assert!(matches!(correct(id, "  "), Err(WriteError::Blank)));
    let nowhere = LineId::from_stored(9999);
    assert!(matches!(correct(nowhere, "x"), Err(WriteError::Missing(id)) if id == nowhere));
    w::no_speech(&f.meaning, &f.ingest, f.root(), id).expect("no speech");
    let hidden = LineId::from_stored(f.hidden()[0].0);
    assert!(matches!(correct(hidden, "x"), Err(WriteError::Hidden(_))));
    assert!(matches!(
        w::undo_correction(&f.meaning, &f.ingest, f.root(), line(&f, "hoe gaat het?")),
        Err(WriteError::NotCorrected(_))
    ));
    assert!(
        f.act_kinds().iter().all(|k| k == "NoSpeech"),
        "nothing else filed"
    );
}

#[test]
fn undoing_a_correction_shows_the_models_words_again() {
    let f = fleet();
    let id = line(&f, "hoe gaat het?");
    w::correct(
        &f.meaning,
        &f.ingest,
        f.root(),
        id,
        "anders",
        &Correction::default(),
    )
    .expect("correct");

    w::undo_correction(&f.meaning, &f.ingest, f.root(), id).expect("undo");

    assert_eq!(f.texts(), ["goede morgen.", "hoe gaat het?"]);
    assert_eq!(f.act_kinds(), ["Words", "Retract"]);
}

#[test]
fn nobody_spoke_hides_the_line_and_its_undo_by_the_hidden_id_shows_it() {
    let f = fleet();
    let id = line(&f, "goede morgen.");

    w::no_speech(&f.meaning, &f.ingest, f.root(), id).expect("no speech");
    assert_eq!(f.texts(), ["hoe gaat het?"]);
    let (hidden, _, reason) = f.hidden()[0].clone();
    assert_eq!(reason, "nobody spoke");
    assert!(matches!(
        w::undo_no_speech(&f.meaning, &f.ingest, f.root(), line(&f, "hoe gaat het?")),
        Err(WriteError::NotNobodySpoke(_))
    ));

    w::undo_no_speech(&f.meaning, &f.ingest, f.root(), LineId::from_stored(hidden)).expect("undo");
    assert_eq!(f.texts(), ["goede morgen.", "hoe gaat het?"]);
}

#[test]
fn a_labelled_fragment_can_be_named_again_or_kept_from_the_voiceprints() {
    let f = fleet();
    let id = line(&f, "goede morgen.");
    w::correct(
        &f.meaning,
        &f.ingest,
        f.root(),
        id,
        "goede morgen.",
        &Correction {
            speaker: Some("Robin"),
            words_checked: false,
        },
    )
    .expect("name");
    let labels = recalld::labels::list_corrections(&f.meaning, None, 50).expect("labels");
    assert_eq!(labels.len(), 1);
    assert_eq!(labels[0].text, "goede morgen.");
    let fragment = EditId::from_stored(labels[0].id);

    w::relabel(&f.meaning, &f.ingest, f.root(), fragment, "Sam").expect("relabel");
    let labels = recalld::labels::list_corrections(&f.meaning, None, 50).expect("labels");
    assert_eq!(labels.len(), 1, "the later naming replaces the earlier");
    assert_eq!(labels[0].speaker.as_deref(), Some("Sam"));

    w::unenrol(
        &f.meaning,
        &f.ingest,
        f.root(),
        EditId::from_stored(labels[0].id),
    )
    .expect("unenrol");
    assert!(
        recalld::labels::list_corrections(&f.meaning, None, 50)
            .expect("labels")
            .is_empty()
    );
    let l = f
        .shown()
        .into_iter()
        .find(|l| l.id == id.stored())
        .expect("line");
    assert_eq!(
        l.label.as_deref(),
        Some("Sam"),
        "the name stays on the line"
    );
    assert!(matches!(
        w::relabel(&f.meaning, &f.ingest, f.root(), EditId::from_stored(9999), "x"),
        Err(WriteError::NotALabel(id)) if id == EditId::from_stored(9999)
    ));
}

#[test]
fn a_span_across_two_clips_is_one_naming_per_clip_cut_at_word_edges() {
    let f = Fleet::new();
    f.clip(
        "meeting-20261004-1200",
        "upload",
        "20261004T120000",
        &[(50.0, 58.0, "een twee drie vier")],
        None,
    );
    f.clip(
        "meeting-20261004-1200",
        "upload",
        "20261004T120100",
        &[(1.0, 9.0, "vijf zes zeven acht")],
        None,
    );
    f.refresh();
    let first = line(&f, "een twee drie vier");
    let last = line(&f, "vijf zes zeven acht");

    // From "drie" (char 9) to the end of "zes" (char 8).
    let touched = w::assign_span(
        &f.meaning,
        &f.ingest,
        f.root(),
        "meeting-20261004-1200",
        (first, 9),
        (last, 8),
        "Robin",
    )
    .expect("assign");

    assert_eq!(touched, 2);
    let named: Vec<(String, Option<String>)> =
        f.shown().into_iter().map(|l| (l.text, l.label)).collect();
    let robin = Some("Robin".to_owned());
    assert_eq!(
        named,
        [
            ("een twee".to_owned(), None),
            ("drie vier".to_owned(), robin.clone()),
            ("vijf zes".to_owned(), robin),
            ("zeven acht".to_owned(), None),
        ]
    );
    let (first, last) = (line(&f, "een twee"), line(&f, "zeven acht"));
    assert!(matches!(
        w::assign_span(
            &f.meaning,
            &f.ingest,
            f.root(),
            "usb",
            (first, 0),
            (last, 1),
            "x"
        ),
        Err(WriteError::BadSpan)
    ));
}

#[test]
fn a_voice_named_for_a_session_shows_on_its_lines_and_clearing_it_takes_it_back() {
    let f = fleet();
    let usb = SourceId::parse("usb").expect("source");

    assert!(
        w::name_voice(
            &f.meaning,
            &f.ingest,
            f.root(),
            &usb,
            "SPEAKER_01",
            Some("Sam")
        )
        .expect("name")
    );
    let labels: Vec<Option<String>> = f.shown().into_iter().map(|l| l.label).collect();
    assert_eq!(labels, [None, Some("Sam".to_owned())]);

    assert!(
        w::name_voice(&f.meaning, &f.ingest, f.root(), &usb, "SPEAKER_01", None).expect("clear")
    );
    assert!(f.shown().into_iter().all(|l| l.label.is_none()));
    assert!(
        !w::name_voice(&f.meaning, &f.ingest, f.root(), &usb, "SPEAKER_01", None).expect("again")
    );
}

#[test]
fn a_pinned_language_is_the_sessions_until_it_is_taken_back() {
    let f = fleet();
    let usb = SourceId::parse("usb").expect("source");
    assert_eq!(w::language_of(&f.meaning, "usb").expect("read"), None);

    w::pin_language(
        &f.meaning,
        &f.ingest,
        f.root(),
        &usb,
        Some(Language::English),
    )
    .expect("pin");
    assert_eq!(
        w::language_of(&f.meaning, "usb").expect("read"),
        Some(Language::English)
    );

    w::pin_language(&f.meaning, &f.ingest, f.root(), &usb, None).expect("unpin");
    assert_eq!(w::language_of(&f.meaning, "usb").expect("read"), None);
}

/// A session of two clips, one line in the first and two in the second, and
/// a household clip beside them.
fn two_clips() -> Fleet {
    let f = Fleet::new();
    f.clip(
        "meeting-20261004-1200",
        "upload",
        "20261004T120000",
        &[(50.0, 58.0, "een twee drie vier")],
        None,
    );
    f.clip(
        "meeting-20261004-1200",
        "upload",
        "20261004T120100",
        &[
            (1.0, 9.0, "vijf zes zeven acht"),
            (20.0, 25.0, "negen tien"),
        ],
        None,
    );
    f.clip(
        "usb",
        "coreaudio",
        "20261004T120100",
        &[(30.0, 32.0, "elders")],
        None,
    );
    f.refresh();
    f
}

#[test]
fn excluding_a_stretch_hides_its_lines_across_clips_and_one_undo_brings_it_all_back() {
    // Strangers talking: excluded as if recording had paused, not filed as
    // invented words.
    let f = two_clips();
    let first = line(&f, "een twee drie vier");
    let last = line(&f, "vijf zes zeven acht");

    let excluded = w::exclude(&f.meaning, &f.ingest, f.root(), first, last).expect("exclude");

    assert_eq!(excluded, 2);
    assert_eq!(f.texts(), ["negen tien", "elders"]);
    let hidden = f.hidden();
    assert_eq!(hidden.len(), 2);
    assert!(
        hidden.iter().all(|(_, _, reason)| reason == "excluded"),
        "{hidden:?}"
    );
    let kinds = f.act_kinds();
    assert_eq!(kinds, ["Exclude", "Exclude"]);

    // Either line of the stretch takes all of it back.
    w::undo_exclude(
        &f.meaning,
        &f.ingest,
        f.root(),
        LineId::from_stored(hidden[1].0),
    )
    .expect("undo");
    assert_eq!(
        f.texts(),
        [
            "een twee drie vier",
            "vijf zes zeven acht",
            "negen tien",
            "elders"
        ]
    );
}

#[test]
fn an_exclusion_must_run_forward_within_one_recording_and_only_one_can_be_undone() {
    let f = two_clips();
    let (first, last) = (line(&f, "een twee drie vier"), line(&f, "negen tien"));
    let elsewhere = line(&f, "elders");

    assert!(matches!(
        w::exclude(&f.meaning, &f.ingest, f.root(), last, first),
        Err(WriteError::BadSpan)
    ));
    assert!(matches!(
        w::exclude(&f.meaning, &f.ingest, f.root(), first, elsewhere),
        Err(WriteError::BadSpan)
    ));
    assert!(matches!(
        w::undo_exclude(&f.meaning, &f.ingest, f.root(), first),
        Err(WriteError::NotExcluded(_))
    ));
}
