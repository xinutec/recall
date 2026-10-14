//! The source kinds the archive stores.

use doctor::source::SourceKind;

#[test]
fn every_kind_round_trips_through_its_stored_spelling() {
    for kind in [SourceKind::CoreAudio, SourceKind::TcpPcm] {
        assert_eq!(SourceKind::parse(kind.as_str()), Some(kind));
    }
}
