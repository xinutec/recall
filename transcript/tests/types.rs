//! The domain types refuse what they promise to refuse.

use transcript::{Instant, SourceId, Span};

fn us(micros: i64) -> Instant {
    Instant::from_micros(micros).expect("in range")
}

#[test]
fn every_stored_spelling_of_one_moment_is_one_instant() {
    // The meaning plane spells `+00:00`, the ingest plane `Z`; a stored
    // offset or a missing one is the same moment.
    let spellings = [
        "2026-09-05T12:00:00+00:00",
        "2026-09-05T12:00:00Z",
        "2026-09-05T14:00:00+02:00",
        "2026-09-05T12:00:00",
        "2026-09-05 12:00:00",
    ];
    let first = Instant::parse(spellings[0]).expect("parses");
    for spelling in spellings {
        assert_eq!(Instant::parse(spelling), Some(first), "{spelling}");
    }
}

#[test]
fn microseconds_are_kept_and_finer_precision_is_refused() {
    let at = Instant::parse("2026-09-05T12:00:00.123456+00:00").expect("parses");
    assert_eq!(at.micros() % 1_000_000, 123_456);
    assert_eq!(Instant::parse("2026-09-05T12:00:00.1234567Z"), None);
    assert_eq!(Instant::parse("now"), None);
}

#[test]
fn an_instant_round_trips_through_chrono() {
    let at = Instant::from_micros(1_757_073_600_123_456).expect("in range");
    assert_eq!(Instant::from_utc(at.to_utc()), at);
    assert_eq!(
        at.plus_seconds(1.5).unwrap().micros() - at.micros(),
        1_500_000
    );
    assert!(Instant::from_micros(i64::MAX).is_none());
    assert!(at.plus_seconds(f64::NAN).is_none());
    assert!(at.plus_seconds(1e300).is_none());
}

#[test]
fn a_span_cannot_run_backwards() {
    let (a, b) = (us(10), us(20));
    assert!(Span::new(b, a).is_none());
    let point = Span::new(a, a).expect("a point is a span");
    assert_eq!(point.micros(), 0);
}

#[test]
fn spans_overlap_only_when_they_share_time() {
    let span = |a, b| Span::new(us(a), us(b)).unwrap();
    assert!(span(0, 10).overlaps(span(5, 15)));
    assert!(
        !span(0, 10).overlaps(span(10, 20)),
        "touching ends do not overlap"
    );
    assert_eq!(span(0, 10).intersection(span(5, 15)), Some(span(5, 10)));
    assert_eq!(span(0, 10).intersection(span(10, 20)), None);
    assert!(span(0, 10).contains(us(0)));
    assert!(!span(0, 10).contains(us(10)));
}

#[test]
fn a_source_id_is_one_safe_path_component() {
    for good in [
        "usb",
        "geb",
        "pixel5",
        "iphone11",
        "pixel-9-3f7a",
        "meeting-20260907-0905",
    ] {
        assert!(SourceId::parse(good).is_some(), "{good}");
    }
    for bad in ["", "Pixel5", "../etc", "a/b", "a.b", "-x", &"a".repeat(65)] {
        assert!(SourceId::parse(bad).is_none(), "{bad:?}");
    }
}

#[test]
fn blank_words_and_blank_names_cannot_be_made() {
    use transcript::{Language, Name, Text};
    assert!(Text::new("   ").is_none());
    assert_eq!(Text::new("  hallo ").unwrap().as_str(), "hallo");
    assert!(Name::new("").is_none());
    assert_eq!(Language::from_code("nl"), Some(Language::Dutch));
    assert_eq!(
        Language::from_code("it"),
        None,
        "only the household's languages can be pinned"
    );
    assert_eq!(Language::English.code(), "en");
}

fn stored_acts() -> Vec<transcript::Act> {
    use transcript::{Act, ClipId, EditId, Language, Name, Text};
    let clip = ClipId::from_stored(7);
    let span = Span::new(us(1_000_000), us(2_500_000)).expect("ordered");
    let source = SourceId::parse("meeting-20260520-1901").expect("source");
    vec![
        Act::Words {
            clip,
            span,
            over: Span::new(us(500_000), us(3_000_000)).expect("ordered"),
            text: Text::new("hallo daar").expect("text"),
            checked: true,
        },
        Act::Speaker {
            clip,
            span,
            name: Name::new("Robin").expect("name"),
            enrol: false,
        },
        Act::NoSpeech { clip, span },
        Act::Unintelligible { clip, span },
        Act::Voice {
            source: source.clone(),
            cluster: "SPEAKER_01".into(),
            name: Name::new("Sam").expect("name"),
        },
        Act::Language {
            source,
            language: Language::Dutch,
        },
        Act::Retract {
            edit: EditId::from_stored(3),
        },
    ]
}

#[test]
fn every_act_reads_back_as_it_was_stored() {
    for act in stored_acts() {
        let json = serde_json::to_string(&act).expect("serialise");
        let back: transcript::Act = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, act, "{json}");
    }
}

#[test]
fn a_stored_act_that_breaks_a_types_promise_is_refused() {
    let speaker = serde_json::to_value(&stored_acts()[1]).expect("serialise");
    let with = |pointer: &str, value: serde_json::Value| {
        let mut v = speaker.clone();
        *v.pointer_mut(pointer).expect("path") = value;
        serde_json::from_value::<transcript::Act>(v)
    };
    assert!(with("/Speaker/name", "  ".into()).is_err(), "an empty name");
    assert!(
        with("/Speaker/span/start", 9_000_000.into()).is_err(),
        "a span ending before it starts"
    );
    assert!(
        with("/Speaker/span/end", i64::MAX.into()).is_err(),
        "an instant out of range"
    );
    let voice = serde_json::to_value(&stored_acts()[4]).expect("serialise");
    let mut bad = voice.clone();
    *bad.pointer_mut("/Voice/source").expect("path") = "Not A Source".into();
    assert!(serde_json::from_value::<transcript::Act>(bad).is_err());
}
