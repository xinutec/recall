//! The pulse writer. The reader's tests (`doctor/tests/verdicts.rs`) parse the
//! same fixture.

use runner::pulse::{stamp_now, stamp_pulse};
use std::time::{Duration, Instant};

const CONTRACT: &str = include_str!("../../tests/fixtures/worker-heartbeat.json");

#[test]
fn the_pulse_carries_every_key_the_doctor_reads() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("worker-heartbeat.json");
    let started = "2026-09-13T18:00:00Z".parse().expect("started");
    stamp_now(&path, started, 3).expect("write");

    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
    let contract: serde_json::Value = serde_json::from_str(CONTRACT).expect("fixture");
    for key in contract.as_object().expect("object").keys() {
        assert!(
            written.get(key).is_some(),
            "the doctor reads `{key}` and the runner does not write it"
        );
    }
    assert_eq!(written["rows"], 3);
    assert!(written["seconds"].as_f64().expect("seconds") >= 0.0);
}

#[test]
fn the_stamps_are_spelled_the_way_the_doctor_parses_them() {
    // Instants are compared as text, so there is one spelling.
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("worker-heartbeat.json");
    stamp_now(&path, "2026-09-13T18:00:00Z".parse().expect("started"), 0).expect("write");
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
    for key in ["started", "finished"] {
        let stamp = written[key].as_str().expect(key);
        assert!(stamp.ends_with("+00:00"), "{key} = {stamp}");
        assert!(!stamp.contains('Z'), "{key} = {stamp}");
    }
}

#[test]
fn no_path_means_no_pulse_and_no_panic() {
    stamp_pulse(None, "2026-09-13T18:00:00Z".parse().expect("started"), 0);
}

/// On an external volume launchd has no grant for, `open()` hangs. A hang
/// cannot be reproduced portably, so this checks what makes one harmless: the
/// caller does not wait for the write.
#[test]
fn stamping_never_makes_the_caller_wait_on_the_filesystem() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("no-such-dir").join("worker-heartbeat.json");
    let started = "2026-09-13T18:00:00Z".parse().expect("started");

    let before = Instant::now();
    for _ in 0..50 {
        stamp_pulse(Some(&path), started, 1);
    }
    let elapsed = before.elapsed();

    assert!(
        elapsed < Duration::from_secs(1),
        "50 stamps took {elapsed:?} — the caller is waiting on the filesystem"
    );
    assert!(!path.exists(), "and the write genuinely could not land");
}

#[test]
fn the_synchronous_form_reports_what_the_filesystem_said() {
    let dir = tempfile::tempdir().expect("tmp");
    let started = "2026-09-13T18:00:00Z".parse().expect("started");

    let good = dir.path().join("worker-heartbeat.json");
    stamp_now(&good, started, 2).expect("a writable path works");
    let body: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&good).expect("read")).expect("json");
    assert_eq!(body["rows"], 2);

    let bad = dir.path().join("no-such-dir").join("worker-heartbeat.json");
    assert!(
        stamp_now(&bad, started, 2).is_err(),
        "an unwritable path is an error, not a silent success"
    );
}

/// Stamps in a loop: the writer is process-wide and latest-wins, so another
/// test's stamp can replace one before it lands.
#[test]
fn stamps_left_for_the_background_writer_keep_the_pulse_ticking() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("worker-heartbeat.json");
    let started = "2026-09-13T18:00:00Z".parse().expect("started");

    let deadline = Instant::now() + Duration::from_secs(10);
    let body = loop {
        stamp_pulse(Some(&path), started, 7);
        if let Ok(body) = std::fs::read_to_string(&path) {
            break body;
        }
        assert!(Instant::now() < deadline, "the pulse never ticked at all");
        std::thread::sleep(Duration::from_millis(20));
    };
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["rows"], 7);
}
