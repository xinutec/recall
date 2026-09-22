//! The archive's timestamp spelling, and reading the spellings it holds.

use audiocore::instant::{parse, parse_utc, python_isoformat, python_isoformat_utc};
use chrono::DateTime;

#[test]
fn the_three_spellings_the_archive_holds_are_the_same_moment() {
    let offset = parse_utc("2026-09-08T19:11:22+00:00").unwrap();
    assert_eq!(parse_utc("2026-09-08T19:11:22Z").unwrap(), offset);
    // Naive: assumed UTC, never refused.
    assert_eq!(parse_utc("2026-09-08T19:11:22").unwrap(), offset);
}

#[test]
fn microseconds_survive() {
    let with = parse_utc("2026-09-08T19:11:22.164504+00:00").unwrap();
    assert_eq!(with.timestamp_subsec_micros(), 164_504);
}

#[test]
fn a_non_utc_offset_names_the_same_instant_and_is_kept_when_respelled() {
    assert_eq!(
        parse_utc("2026-09-08T20:11:22+01:00").unwrap(),
        parse_utc("2026-09-08T19:11:22Z").unwrap()
    );
    assert_eq!(
        parse("2026-09-08T20:11:22+01:00")
            .unwrap()
            .offset()
            .local_minus_utc(),
        3600
    );
    assert_eq!(
        python_isoformat("2026-09-08T20:11:22+01:00").as_deref(),
        Some("2026-09-08T20:11:22+01:00")
    );
}

#[test]
fn junk_is_none_rather_than_a_panic() {
    assert!(parse("").is_none());
    assert!(parse("not a time").is_none());
    assert!(python_isoformat("not a time").is_none());
}

#[test]
fn a_zero_fraction_is_dropped_and_a_real_one_kept() {
    // Compared as text by every archive query, so the spelling is the one
    // every stored row already has.
    let whole = DateTime::from_timestamp(1_788_894_682, 0).unwrap();
    assert_eq!(python_isoformat_utc(whole), "2026-09-08T19:11:22+00:00");
    let fractional = DateTime::from_timestamp(1_788_894_682, 164_504_000).unwrap();
    assert_eq!(
        python_isoformat_utc(fractional),
        "2026-09-08T19:11:22.164504+00:00"
    );
    assert_eq!(
        python_isoformat("2026-09-08T19:11:22Z").as_deref(),
        Some("2026-09-08T19:11:22+00:00")
    );
    assert_eq!(
        python_isoformat("2026-09-08T19:11:22.500Z").as_deref(),
        Some("2026-09-08T19:11:22.500000+00:00")
    );
}
