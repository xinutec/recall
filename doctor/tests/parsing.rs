//! Reading what the archive stores: instants Python wrote, and source kinds.

use doctor::instant::parse;
use doctor::source::SourceKind;

#[test]
fn the_three_spellings_the_archive_holds_are_the_same_moment() {
    let offset = parse("2026-09-08T19:11:22+00:00").unwrap();
    assert_eq!(parse("2026-09-08T19:11:22Z").unwrap(), offset);
    // Naive: assumed UTC, never refused.
    assert_eq!(parse("2026-09-08T19:11:22").unwrap(), offset);
}

#[test]
fn microseconds_survive() {
    let with = parse("2026-09-08T19:11:22.164504+00:00").unwrap();
    assert_eq!(with.timestamp_subsec_micros(), 164_504);
}

#[test]
fn a_non_utc_offset_names_the_same_instant() {
    assert_eq!(
        parse("2026-09-08T20:11:22+01:00").unwrap(),
        parse("2026-09-08T19:11:22Z").unwrap()
    );
}

#[test]
fn junk_is_none_rather_than_a_panic() {
    assert!(parse("").is_none());
    assert!(parse("not a time").is_none());
}

#[test]
fn every_kind_round_trips_through_its_stored_spelling() {
    for kind in [
        SourceKind::CoreAudio,
        SourceKind::Lavfi,
        SourceKind::Rtsp,
        SourceKind::TcpPcm,
        SourceKind::Upload,
        SourceKind::Discovered,
    ] {
        assert_eq!(SourceKind::parse(kind.as_str()), Some(kind));
    }
}

#[test]
fn only_the_two_recorderless_kinds_are_excluded_from_device_checks() {
    assert!(SourceKind::CoreAudio.is_device());
    assert!(SourceKind::TcpPcm.is_device());
    assert!(!SourceKind::Upload.is_device());
    assert!(!SourceKind::Discovered.is_device());
}
