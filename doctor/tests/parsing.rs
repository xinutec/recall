//! The source kinds the archive stores.

use doctor::source::SourceKind;

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
