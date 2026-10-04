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
