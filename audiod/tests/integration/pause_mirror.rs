//! The pause mirror's decision rule, the part that can clobber a local pause:
//! edge-triggering, the unparseable-value trap, and the conservative reading
//! of an elapsed bound.

use audiod::pause_mirror::{Decision, Desired, decide, source_liveness};
use chrono::{DateTime, Utc};

fn at(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .expect("timestamp")
        .with_timezone(&Utc)
}

const NOW: &str = "2026-09-11T12:00:00Z";
const LATER: &str = "2026-09-11T18:00:00+00:00";
const EARLIER: &str = "2026-09-11T06:00:00+00:00";

#[test]
fn an_unchanged_intent_touches_nothing() {
    // Why the mirror is edge-triggered: reapplying an unchanged "running" every
    // tick would silently clobber a pause pressed on the Mac's own LAN UI.
    assert_eq!(decide("", None, at(NOW)), Decision::Unchanged);
    assert_eq!(decide(LATER, Some(LATER), at(NOW)), Decision::Unchanged);
}

#[test]
fn a_new_pause_is_applied_and_recorded() {
    let Decision::Apply { desired, record } = decide("", Some(LATER), at(NOW)) else {
        panic!("a changed intent must apply");
    };
    assert_eq!(desired, Desired::PausedUntil(LATER.to_owned()));
    assert_eq!(record, LATER, "the marker records what the fleet said");
}

#[test]
fn a_cleared_intent_resumes() {
    let Decision::Apply { desired, record } = decide(LATER, None, at(NOW)) else {
        panic!("clearing a pause must apply");
    };
    assert_eq!(desired, Desired::Running);
    assert_eq!(record, "");
}

#[test]
fn an_elapsed_bound_reads_as_running() {
    // A bounded pause that has expired is not a pause, as `paused_until` and
    // `intent_until` also read it.
    let Decision::Apply { desired, .. } = decide("", Some(EARLIER), at(NOW)) else {
        panic!("must apply");
    };
    assert_eq!(desired, Desired::Running);
}

#[test]
fn an_unparseable_intent_is_recorded_so_it_is_never_retried_forever() {
    // A bad value reads as running and is also recorded; without the record the
    // mirror retries it every tick and never reaches a real pause pressed later.
    let Decision::Apply { desired, record } = decide("", Some("not-a-time"), at(NOW)) else {
        panic!("must apply");
    };
    assert_eq!(desired, Desired::Running);
    assert_eq!(record, "not-a-time", "the bad value is recorded as seen");
    // Once recorded, the next pass is a no-op rather than a retry.
    assert_eq!(
        decide("not-a-time", Some("not-a-time"), at(NOW)),
        Decision::Unchanged
    );
}

#[test]
fn liveness_reads_the_marker_mtime_not_its_contents() {
    // `.alive` is EMPTY by design: its mtime is the measurement.
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join("usb")).expect("mkdir");
    std::fs::write(dir.path().join("usb/.alive"), b"").expect("marker");
    std::fs::create_dir(dir.path().join("no-marker")).expect("mkdir");

    let live = source_liveness(dir.path());
    assert!(live.contains_key("usb"), "{live:?}");
    assert!(
        !live.contains_key("no-marker"),
        "a source with no marker reports nothing, rather than a false time"
    );
    let stamp = live["usb"].as_str().expect("iso string");
    assert!(stamp.parse::<DateTime<Utc>>().is_ok(), "{stamp}");
}

#[test]
fn an_unreadable_root_is_empty_not_a_panic() {
    // Liveness is best-effort; a mirror that died reading it would stop applying pauses.
    assert!(source_liveness(std::path::Path::new("/nonexistent-xyz")).is_empty());
}
