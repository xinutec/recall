//! The bytes between recall's processes, recorded from the copies these types
//! replaced: each fixture must read and write back unchanged.

use std::collections::BTreeMap;

use audiocore::capture::{Applied, CaptureState, Intent};
use audiocore::devices::Heartbeat;
use audiocore::ingest::Receipt;
use audiocore::live_tier::{Heard, LiveHealth};
use audiocore::work::{Lease, LiveStored, LiveTurns, Prompt};
use serde::Serialize;
use serde::de::DeserializeOwned;

fn round_trips<T: Serialize + DeserializeOwned>(fixture: &str) -> T {
    let path = format!(
        "{}/../tests/fixtures/wire/{fixture}",
        env!("CARGO_MANIFEST_DIR")
    );
    let recorded = std::fs::read_to_string(&path).expect("fixture");
    let recorded = recorded.trim_end();
    let read: T = serde_json::from_str(recorded).expect("reads");
    assert_eq!(
        serde_json::to_string(&read).expect("writes"),
        recorded,
        "{fixture}"
    );
    read
}

#[test]
fn a_lease_writes_what_the_queue_sent() {
    for fixture in ["lease-enrol.json", "lease-pinned.json", "lease-plain.json"] {
        assert!(round_trips::<Lease>(fixture).job.is_some(), "{fixture}");
    }
    assert_eq!(round_trips::<Lease>("lease-empty.json").job, None);
}

#[test]
fn live_turns_and_their_answer_write_what_the_runner_and_recalld_sent() {
    let pushed = round_trips::<LiveTurns>("live-turns.json");
    assert_eq!(pushed.turns[1].language, None);
    assert_eq!(round_trips::<LiveStored>("live-stored.json").stored, 2);
}

#[test]
fn the_prompt_is_null_when_the_glossary_is_empty() {
    assert!(round_trips::<Prompt>("prompt.json").prompt.is_some());
    assert_eq!(round_trips::<Prompt>("prompt-empty.json").prompt, None);
}

#[test]
fn a_turn_without_a_language_reads_as_the_models_guess() {
    let turns: LiveTurns =
        serde_json::from_str(r#"{"turns":[{"start":"a","end":"b","text":"c","asr_model":"d"}]}"#)
            .expect("reads");
    assert_eq!(turns.turns[0].language, None);
}

#[test]
fn live_health_writes_what_recalld_sent() {
    assert_eq!(
        round_trips::<LiveHealth>("live-health.json").lag_samples,
        12
    );
    assert_eq!(
        round_trips::<LiveHealth>("live-health-quiet.json").lag_median_s,
        None
    );
}

#[test]
fn heard_writes_what_recalld_sent() {
    let heard = round_trips::<Vec<Heard>>("heard.json");
    assert_eq!(heard[1], Heard::new("usb", 300.0, 0.0));
    assert!((heard[0].per_minute() - 126.5 * 60.0 / 299.965).abs() < 1e-9);
}

#[test]
fn the_capture_state_writes_what_recalld_sent() {
    let paused = round_trips::<CaptureState>("capture-state-paused.json");
    assert!(!paused.desired_running);
    assert!(round_trips::<CaptureState>("capture-state-running.json").desired_running);
}

#[test]
fn the_mirror_report_and_its_answer_write_what_was_sent() {
    let applied = round_trips::<Applied>("capture-applied.json");
    assert_eq!(applied.source_liveness.len(), 2);
    assert_eq!(
        round_trips::<Applied>("capture-applied-running.json").known_intent,
        None
    );
    assert!(
        round_trips::<Intent>("capture-intent.json")
            .paused_until
            .is_some()
    );
    assert_eq!(
        round_trips::<Intent>("capture-intent-running.json").paused_until,
        None
    );
}

#[test]
fn a_bare_report_reads_as_running_with_nothing_live() {
    let bare: Applied = serde_json::from_str(r#"{"running":true}"#).expect("reads");
    assert_eq!(bare.source_liveness, BTreeMap::new());
    assert_eq!((bare.paused_until, bare.known_intent), (None, None));
}

#[test]
fn the_receipt_writes_what_recalld_sent() {
    assert_eq!(round_trips::<Receipt>("receipt.json").bytes, 961_234);
}

#[test]
fn a_relayed_heartbeat_writes_what_the_relay_sent() {
    let full = round_trips::<Heartbeat>("heartbeat-relayed.json");
    assert_eq!(full.via_lan, Some(true));
    let bare = round_trips::<Heartbeat>("heartbeat-relayed-bare.json");
    assert_eq!((bare.app, bare.streaming), (None, None));
}
