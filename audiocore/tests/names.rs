//! The name grammar is the timing contract: a refusal here is a recorder bug
//! caught at the door, not a mis-stamped row downstream.

use audiocore::names::{Extension, NameError, parse, valid_source};

#[test]
fn a_wellformed_name_decomposes() {
    let name = parse("usb", "usb-20260905T120000.flac").expect("valid");
    assert_eq!(name.source, "usb");
    assert_eq!(name.start_utc, "2026-09-05T12:00:00Z");
    assert_eq!(name.ext, Extension::Flac);
}

#[test]
fn every_recorder_extension_is_accepted() {
    for ext in ["flac", "opus", "ogg", "wav"] {
        assert!(parse("usb", &format!("usb-20260905T120000.{ext}")).is_ok());
    }
}

#[test]
fn a_phones_own_copy_is_named_apart_from_its_streams() {
    // The host cuts a phone's stream and the phone keeps its own copy of the
    // same minute; both are FLAC and often open in the same second.
    let own = parse("pixel5", "pixel5-20261003T120000.phone.flac").expect("valid");
    assert_eq!(own.start_utc, "2026-10-03T12:00:00Z");
    assert_eq!(own.ext, Extension::Flac);
    assert_eq!(
        parse("pixel5", "pixel5-20261003T120000.tablet.flac"),
        Err(NameError::BadExtension)
    );
}

#[test]
fn the_prefix_must_be_the_source() {
    assert_eq!(
        parse("usb", "geb-20260905T120000.flac"),
        Err(NameError::WrongPrefix)
    );
}

#[test]
fn an_impossible_instant_is_refused() {
    // Month 13: the stamp must be a real UTC instant, not just fourteen digits.
    assert_eq!(
        parse("usb", "usb-20261305T120000.flac"),
        Err(NameError::BadStamp)
    );
}

#[test]
fn a_short_stamp_is_refused() {
    assert_eq!(parse("usb", "usb-2026.flac"), Err(NameError::BadStamp));
}

#[test]
fn unknown_extensions_are_refused() {
    assert_eq!(
        parse("usb", "usb-20260905T120000.aiff"),
        Err(NameError::BadExtension)
    );
}

#[test]
fn an_uploaded_recordings_container_is_a_deliverable_one() {
    // An upload is fetched back through `/ingest/v1/blob`, which parses the
    // name, so a voice memo's container must be in the grammar.
    for ext in ["mp3", "m4a", "mp4", "aac", "webm"] {
        let name = parse(
            "meeting-20260907-0905",
            &format!("meeting-20260907-0905-20260907T080526.{ext}"),
        )
        .unwrap_or_else(|_| panic!(".{ext} must be accepted"));
        assert_eq!(name.start_utc, "2026-09-07T08:05:26Z");
    }
}

#[test]
fn a_source_is_a_single_safe_path_component() {
    for bad in ["", "../usb", "a/b", "USB", "usb.", ".usb", "-usb"] {
        assert!(!valid_source(bad), "{bad:?} must be refused");
    }
    for good in ["usb", "geb", "pixel5", "iphone11", "room", "pixel-9-3f7a"] {
        assert!(valid_source(good), "{good:?} must be accepted");
    }
}
