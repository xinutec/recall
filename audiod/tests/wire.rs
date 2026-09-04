//! The handshake is the cross-language contract: the fixtures here are copies
//! of what the phone apps and the Linux mic actually send, so a parser change
//! that would strand a mic fails here first.

use audiod::wire::{HandshakeError, SAMPLE_RATE, parse_handshake, read_handshake};
use std::io::{Cursor, Read};

#[test]
fn full_handshake_parses() {
    let hs = parse_handshake(r#"{"id":"pixel9","rate":48000,"channels":1,"epoch":1757000000.25}"#)
        .expect("valid");
    assert_eq!(hs.source_id, "pixel9");
    assert_eq!(hs.sample_rate, 48_000);
    assert_eq!(hs.channels, 1);
    assert_eq!(hs.epoch, Some(1_757_000_000.25));
}

#[test]
fn rate_and_channels_default_to_48k_mono() {
    let hs = parse_handshake(r#"{"id":"kitchen"}"#).expect("valid");
    assert_eq!(hs.sample_rate, SAMPLE_RATE);
    assert_eq!(hs.channels, 1);
    assert_eq!(hs.epoch, None);
}

#[test]
fn unsafe_ids_are_rejected() {
    for id in ["../etc", "UPPER", "-leading", "", "a b", "h\u{e9}b"] {
        // Proper JSON encoding, so each rejection is the id check itself and
        // not a malformed line passing the test for the wrong reason.
        let line = serde_json::json!({ "id": id }).to_string();
        assert!(parse_handshake(&line).is_none(), "{id:?} accepted");
    }
}

#[test]
fn garbage_epoch_is_tolerated_not_fatal() {
    let hs = parse_handshake(r#"{"id":"geb","epoch":"soon"}"#).expect("valid");
    assert_eq!(hs.epoch, None);
}

#[test]
fn malformed_lines_are_rejected() {
    for line in [
        "",
        "not json",
        r#"{"rate":48000}"#,
        r#"{"id":"x","rate":"fast"}"#,
        r#"{"id":"x","rate":0}"#,
    ] {
        assert!(parse_handshake(line).is_none(), "{line:?} accepted");
    }
}

#[test]
fn read_handshake_stops_at_newline_leaving_pcm_untouched() {
    let mut cursor = Cursor::new(b"{\"id\":\"usb\"}\nPCMPCM".to_vec());
    let hs = read_handshake(&mut cursor).expect("valid");
    assert_eq!(hs.source_id, "usb");
    let mut rest = Vec::new();
    cursor.read_to_end(&mut rest).unwrap();
    assert_eq!(rest, b"PCMPCM"); // not one byte of PCM consumed
}

#[test]
fn read_handshake_names_each_way_of_failing() {
    assert!(matches!(
        read_handshake(&mut Cursor::new(vec![])),
        Err(HandshakeError::Eof)
    ));
    assert!(matches!(
        read_handshake(&mut Cursor::new(vec![b'x'; 10_000])),
        Err(HandshakeError::Overflow)
    ));
    assert!(matches!(
        read_handshake(&mut Cursor::new(b"not json\n".to_vec())),
        Err(HandshakeError::Malformed)
    ));
}
