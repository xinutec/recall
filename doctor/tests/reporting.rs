//! The report fleetwatch receives, and where the token comes from.

use chrono::{DateTime, Utc};
use doctor::check::{Verdict, check};
use doctor::fleetwatch::{mint_ulid, payload, read_token};

#[test]
fn a_ulid_is_twenty_six_crockford_characters() {
    // fleetwatch uses it as the idempotency key and rejects anything that is
    // not one (422), so the spelling is a contract, not a detail.
    const CROCKFORD: &str = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
    let ulid = mint_ulid(now, [0u8; 10]);
    assert_eq!(ulid.len(), 26);
    assert!(
        ulid.ends_with("0000000000000000"),
        "80 zero bits of randomness"
    );
    assert!(ulid.chars().all(|c| CROCKFORD.contains(c)), "{ulid}");
}

#[test]
fn the_timestamp_half_orders_the_way_time_does() {
    let early = mint_ulid(DateTime::from_timestamp(1_700_000_000, 0).unwrap(), [0; 10]);
    let late = mint_ulid(DateTime::from_timestamp(1_700_000_001, 0).unwrap(), [0; 10]);
    assert!(early < late);
}

#[test]
fn the_randomness_reaches_the_key() {
    let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
    assert_ne!(mint_ulid(now, [0; 10]), mint_ulid(now, [255; 10]));
}

#[test]
fn the_payload_carries_every_field_of_the_report_contract() {
    let checks = vec![
        check(
            "capture",
            "recording",
            Verdict::Pass,
            "3/3 microphones live",
            "audio within 5 min",
        )
        .trend(1.5, "min")
        .build(),
    ];
    let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
    let body = payload(&checks, now, Some(1234));
    assert_eq!(body["schema"], 1);
    assert_eq!(body["collector"], "recall");
    // Must match the launchd agent's StartInterval, or a healthy producer is
    // reported as late.
    assert_eq!(body["interval_s"], 300);
    assert_eq!(body["duration_ms"], 1234);
    // `source` is deliberately absent: fleetwatch stamps it from the token, so
    // a producer can only ever write as itself.
    assert!(body.get("source").is_none());
    let check = &body["checks"][0];
    assert_eq!(check["section"], "capture");
    assert_eq!(check["verdict"], "pass");
    assert_eq!(check["value"], 1.5);
    assert_eq!(check["unit"], "min");
}

#[test]
fn a_check_with_nothing_to_trend_sends_nulls_not_omissions() {
    let checks = vec![
        check(
            "archive",
            "archive answers",
            Verdict::Fail,
            "no answer in 60s",
            "x",
        )
        .build(),
    ];
    let body = payload(&checks, Utc::now(), None);
    assert!(body["checks"][0]["value"].is_null());
    assert!(body["checks"][0]["unit"].is_null());
}

#[test]
fn no_token_anywhere_means_not_a_producer_rather_than_a_crash() {
    // This machine is simply not a producer yet — a thing to say plainly.
    let home = tempfile::tempdir().unwrap();
    assert!(read_token(home.path(), None).is_none());
}

#[test]
fn a_token_file_is_read_without_its_surrounding_whitespace() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(".config").join("fleetwatch");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("token"), "  secret\n").unwrap();
    assert_eq!(read_token(home.path(), None).as_deref(), Some("secret"));
}

#[test]
fn an_empty_token_is_no_token_at_all_from_either_source() {
    // The fleet's secret.sh can leave the file there before it has anything to
    // put in it, and an empty Bearer header would read as a credential mistake.
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(".config").join("fleetwatch");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("token"), "\n").unwrap();
    assert!(read_token(home.path(), None).is_none());
    assert!(read_token(home.path(), Some("  ")).is_none());
}

#[test]
fn the_environment_wins_over_the_file() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(".config").join("fleetwatch");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("token"), "from-the-file").unwrap();
    assert_eq!(
        read_token(home.path(), Some("from-the-env")).as_deref(),
        Some("from-the-env")
    );
}
