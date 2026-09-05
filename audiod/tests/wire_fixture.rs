//! The cross-language handshake contract: every line in `handshakes.json` is
//! what a real client emits (Python mic, Android, iOS, a pre-epoch app), and
//! the LIVE parser must accept every one with exactly these fields. The
//! Python side (`tests/test_mic.py`) asserts its client still produces its
//! line byte for byte — so a drift on either end of the wire fails a gate.

use audiod::wire::parse_handshake;

#[test]
fn every_client_line_parses_with_its_declared_fields() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("handshakes.json")).expect("fixture parses");
    let cases = fixture["cases"].as_array().expect("cases array");
    assert!(!cases.is_empty());
    for case in cases {
        let client = case["client"].as_str().unwrap();
        let line = case["line"].as_str().unwrap();
        let hs = parse_handshake(line)
            .unwrap_or_else(|| panic!("{client}: line rejected by the live parser"));
        assert_eq!(hs.source_id, case["id"].as_str().unwrap(), "{client}: id");
        assert_eq!(
            i64::from(hs.sample_rate),
            case["rate"].as_i64().unwrap(),
            "{client}: rate"
        );
        assert_eq!(
            i64::from(hs.channels),
            case["channels"].as_i64().unwrap(),
            "{client}: channels"
        );
        assert_eq!(hs.epoch, case["epoch"].as_f64(), "{client}: epoch");
    }
}
