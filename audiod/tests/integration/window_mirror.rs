//! The mirror's decision rule, the part that can clobber a local change:
//! edge-triggering and the unparseable-value trap.

use audiocore::capture::Window;
use audiod::window_mirror::{Decision, decide, source_liveness};
use chrono::{DateTime, Utc};

fn at(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .expect("timestamp")
        .with_timezone(&Utc)
}

const OPEN: &str = "record 2026-09-11T18:00:00Z";

#[test]
fn an_unchanged_intent_touches_nothing() {
    // Edge-triggered, or an unchanged intent clobbers a break-glass change.
    assert_eq!(decide("off", "off"), Decision::Unchanged);
    assert_eq!(decide(OPEN, OPEN), Decision::Unchanged);
}

#[test]
fn a_new_window_is_applied_and_recorded() {
    let Decision::Apply { window, record } = decide("off", OPEN) else {
        panic!("a changed intent must apply");
    };
    assert_eq!(window, Some(Window::Record(at("2026-09-11T18:00:00Z"))));
    assert_eq!(record, OPEN, "the marker records what the fleet said");
}

#[test]
fn stopping_is_applied() {
    let Decision::Apply { window, record } = decide(OPEN, "off") else {
        panic!("stopping must apply");
    };
    assert_eq!(window, Some(Window::Off));
    assert_eq!(record, "off");
}

/// A marker from before windows (a pause's resume-by, or blank) is a change.
#[test]
fn the_first_window_replaces_an_older_marker() {
    assert!(matches!(decide("", "off"), Decision::Apply { .. }));
    assert!(matches!(
        decide("2026-09-11T18:00:00+00:00", "off"),
        Decision::Apply { .. }
    ));
}

#[test]
fn an_unparseable_intent_is_a_fault_recorded_so_it_is_never_retried_forever() {
    let Decision::Apply { window, record } = decide("off", "not-a-window") else {
        panic!("must apply");
    };
    assert_eq!(window, None, "a fault, which records");
    assert_eq!(record, "not-a-window", "the bad value is recorded as seen");
    assert_eq!(decide("not-a-window", "not-a-window"), Decision::Unchanged);
}

#[test]
fn liveness_reads_the_marker_mtime_not_its_contents() {
    // `.alive` is empty by design: its mtime is the measurement.
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
    let stamp = &live["usb"];
    assert!(stamp.parse::<DateTime<Utc>>().is_ok(), "{stamp}");
}

#[test]
fn an_unreadable_root_is_empty_not_a_panic() {
    // Liveness is best-effort: reading it must not kill the mirror.
    assert!(source_liveness(std::path::Path::new("/nonexistent-xyz")).is_empty());
}
