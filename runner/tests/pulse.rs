//! The archive pulse the DOCTOR reads — the writer's half of a contract whose
//! two halves are in different languages.
//!
//! ⚠ **This test replaces `tests/test_cli_worker.py`, which is deleted with
//! `worker.py` (#1538).** The behaviour did not go away; it moved. The Python
//! test asserted the worker wrote these keys and the doctor's test parses the
//! same fixture — so removing the Python side without writing this one would
//! leave the contract with a reader and no writer, and nothing would say so
//! until the doctor started reporting a Mac that had stopped transcribing.

use runner::pulse::{stamp_now, stamp_pulse};
use std::time::{Duration, Instant};

/// The fixture the DOCTOR's own test parses. Named here on purpose: if the
/// shape ever diverges, the two halves must fail together rather than one of
/// them quietly describing a file nobody writes.
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
    // ⚠ `datetime.isoformat()`: an OFFSET, not `Z`. The doctor reads these with
    // the Python-isoformat reader, and this archive has already paid once for
    // two spellings of one instant — `start_utc` compared as TEXT matched
    // nothing across the two planes. Hand-formatting here is how that recurs.
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
    // A runner not beside the archive it would be certifying must not invent a
    // heartbeat. `None` is that case, and it has to be silent.
    stamp_pulse(None, "2026-09-13T18:00:00Z".parse().expect("started"), 0);
}

/// ⚠ **The pulse must not be able to stop the worker, and this is the test that
/// says so.** On 2026-09-14 the runner finished a job, called `stamp_pulse`, and
/// sat inside `open()` for two hours: `/Volumes/Backup` is an external volume a
/// launchd process has no write grant for, and the syscall HANGS rather than
/// answering `EPERM` (#1618). Process up, shim alive, work queued, nothing in
/// the log.
///
/// A hang cannot be reproduced portably, so this pins the property that makes a
/// hang survivable: the caller does not wait for the write. An unwritable target
/// stands in for an unreachable one — the assertion is on the CALLER's latency,
/// not on the outcome.
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

/// The synchronous form still exists and still reports failure, because a
/// caller that WANTS the answer should be able to have it — the agent simply
/// is not such a caller.
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

/// The asynchronous form does eventually deliver — non-blocking must not mean
/// never-arriving.
///
/// ⚠ **It STAMPS IN A LOOP, and that is the design rather than a flaky test.**
/// The writer is process-global and latest-wins, so a single beat may legitimately
/// be superseded before it lands — by the next job, or here by the sibling test
/// firing 50 stamps at an unwritable path. Asserting that one particular stamp
/// arrives asserts a guarantee this module deliberately does not make, and it
/// failed in the nix sandbox for exactly that reason while passing locally on
/// timing. What IS guaranteed is that stamping keeps the pulse ticking.
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
