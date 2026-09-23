//! The archive pulse the doctor reads: the writer's half of a contract whose
//! reader's half is `doctor/tests/verdicts.rs`, both against one shared fixture.

use runner::pulse::{stamp_now, stamp_pulse};
use std::time::{Duration, Instant};

/// The fixture the doctor's test parses, so a diverging shape fails both halves.
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
    // An offset, not `Z`: the archive compares instants as text, so two
    // spellings of one instant fail to match.
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
    // A runner not beside the archive must not invent a heartbeat; `None` is
    // that case, and it is silent.
    stamp_pulse(None, "2026-09-13T18:00:00Z".parse().expect("started"), 0);
}

/// The pulse must not be able to stop the worker. On an external volume a
/// launchd process has no write grant for, `open()` hangs rather than returning
/// `EPERM`. A hang cannot be reproduced portably, so this pins the property that
/// makes one survivable: the caller does not wait for the write. The assertion
/// is on the caller's latency, not the outcome.
#[test]
fn stamping_never_makes_the_caller_wait_on_the_filesystem() {
    let dir = tempfile::tempdir().expect("tmp");
    // A path whose parent does not exist: the write cannot succeed.
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

/// The synchronous form reports failure, for a caller that wants the answer.
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

/// The asynchronous form does eventually deliver. It stamps in a loop by
/// design: the writer is process-global and latest-wins, so one beat may be
/// superseded before it lands (here by the sibling test's 50 stamps). What is
/// guaranteed is that stamping keeps the pulse ticking.
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
