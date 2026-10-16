//! The live tier's bytes, recorded from the two copies these types replaced:
//! each fixture must read and write back unchanged.

use audiocore::live_tier::{Heard, LiveHealth};
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
