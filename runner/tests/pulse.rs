//! The archive pulse the DOCTOR reads — the writer's half of a contract whose
//! two halves are in different languages.
//!
//! ⚠ **This test replaces `tests/test_cli_worker.py`, which is deleted with
//! `worker.py` (#1538).** The behaviour did not go away; it moved. The Python
//! test asserted the worker wrote these keys and the doctor's test parses the
//! same fixture — so removing the Python side without writing this one would
//! leave the contract with a reader and no writer, and nothing would say so
//! until the doctor started reporting a Mac that had stopped transcribing.

use runner::pulse::stamp_pulse;

/// The fixture the DOCTOR's own test parses. Named here on purpose: if the
/// shape ever diverges, the two halves must fail together rather than one of
/// them quietly describing a file nobody writes.
const CONTRACT: &str = include_str!("../../tests/fixtures/worker-heartbeat.json");

#[test]
fn the_pulse_carries_every_key_the_doctor_reads() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("worker-heartbeat.json");
    let started = "2026-09-13T18:00:00Z".parse().expect("started");
    stamp_pulse(Some(&path), started, 3);

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
    stamp_pulse(
        Some(&path),
        "2026-09-13T18:00:00Z".parse().expect("started"),
        0,
    );
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
