//! The port decides every body the way `recall.beat_relay` did.
//!
//! ⚠ The fixture's verdicts were produced BY the Python, through
//! `scripts/gen_beat_relay_parity.py` — both DELETED in the same commit that
//! added this file, because a generator that imports a module nobody ships is
//! dead code and a fixture nobody can regenerate is the honest state. So this is
//! a frozen regression corpus now, not a live differential, and its provenance
//! is this paragraph plus that commit. It pins the Python's behaviour at the
//! moment of the port — including the parts shared examples cannot reach: which
//! keys survive the allowlist, what a device name of exactly the limit does, and
//! whether a non-object JSON value is a rejection or a crash.
//!
//! This is the boundary between an unauthenticated LAN caller and the fleet's
//! store, which is why it is held to a differential rather than to a handful of
//! cases written alongside the code.

use audiod::beat_relay::{Head, read_head, relayed};
use serde::Deserialize;

#[derive(Deserialize)]
struct Case {
    body: String,
    accepted: bool,
    #[serde(default)]
    forwarded: Option<serde_json::Value>,
}

#[test]
fn the_rust_filter_decides_every_body_the_way_the_python_did() {
    let cases: Vec<Case> =
        serde_json::from_str(include_str!("fixtures/beat-relay-parity.json")).expect("fixture");
    assert!(cases.len() >= 21, "the corpus must not silently shrink");
    let accepted = cases.iter().filter(|c| c.accepted).count();
    assert!(
        accepted > 3 && accepted < cases.len() - 3,
        "a corpus that is all one verdict passes a filter that always answers it"
    );
    for (i, case) in cases.iter().enumerate() {
        let got = relayed(case.body.as_bytes());
        assert_eq!(
            got.is_ok(),
            case.accepted,
            "case {i} ({:?}): accepted disagrees",
            case.body
        );
        if let (Ok(out), Some(want)) = (&got, &case.forwarded) {
            assert_eq!(out, want, "case {i}: forwarded body differs");
        }
    }
}

#[test]
fn the_phone_cannot_assert_the_two_fields_it_does_not_own() {
    // `at` is the fleet's clock, so a beat cannot backdate itself; `viaLan` is
    // this relay's testimony, not the phone's. Both are dropped and `viaLan` is
    // then stamped true — which is why sending `false` must not survive.
    let out = relayed(br#"{"device":"pixel5","at":"2020-01-01T00:00:00Z","viaLan":false}"#)
        .expect("accepted");
    assert!(
        out.get("at").is_none(),
        "a phone must not stamp its own time"
    );
    assert_eq!(out["viaLan"], serde_json::Value::Bool(true));
}

#[test]
fn an_unknown_key_cannot_reach_the_fleet_by_being_added_to_the_app() {
    let out = relayed(br#"{"device":"pixel5","admin":true}"#).expect("accepted");
    assert!(out.get("admin").is_none(), "the allowlist is the boundary");
}

#[test]
fn the_request_head_is_bounded_against_a_caller_that_never_stops() {
    // ⚠ An unauthenticated caller can open a socket and send headers forever.
    // "read until a blank line" alone is that hole.
    let endless = "POST /api/devices/heartbeat HTTP/1.1\r\n".to_owned()
        + &"X-Pad: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\n".repeat(1000);
    let mut reader = std::io::BufReader::new(endless.as_bytes());
    assert!(
        read_head(&mut reader).is_err(),
        "an oversized head must be refused, not accumulated"
    );
}

#[test]
fn a_well_formed_head_parses_to_its_route_and_length() {
    let raw = "POST /api/devices/heartbeat HTTP/1.1\r\nHost: x\r\nContent-Length: 42\r\n\r\n";
    let mut reader = std::io::BufReader::new(raw.as_bytes());
    assert_eq!(
        read_head(&mut reader).expect("head"),
        Head {
            method: "POST".to_owned(),
            path: "/api/devices/heartbeat".to_owned(),
            content_length: 42,
        }
    );
}

#[test]
fn the_content_length_header_is_matched_case_insensitively() {
    // Curl sends `Content-Length`; some clients send `content-length`. Reading
    // only one spelling drops the body and answers 400 to a valid beat.
    let raw = "POST /api/devices/heartbeat HTTP/1.1\r\ncontent-length: 7\r\n\r\n";
    let mut reader = std::io::BufReader::new(raw.as_bytes());
    assert_eq!(read_head(&mut reader).expect("head").content_length, 7);
}
