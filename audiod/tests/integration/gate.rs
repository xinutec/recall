use audiocore::capture::Window;
use audiod::gate::{GATE_FILE, records, window};
use chrono::{DateTime, Duration, Utc};

fn at(s: &str) -> DateTime<Utc> {
    s.parse().expect("instant")
}

fn root_with(content: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(GATE_FILE), content).unwrap();
    dir
}

#[test]
fn off_records_nothing() {
    let dir = root_with("off");
    assert!(!records(dir.path(), Utc::now()));
}

#[test]
fn an_open_window_records_until_its_end() {
    let now = at("2026-10-23T10:00:00Z");
    let dir = root_with("record 2026-10-23T11:00:00Z");
    assert!(records(dir.path(), now));
    assert!(!records(dir.path(), now + Duration::hours(1)));
}

#[test]
fn a_paused_window_records_nothing() {
    let dir = root_with("hold 2030-01-01T00:00:00Z");
    assert!(!records(dir.path(), Utc::now()));
}

/// A fault records: a broken or missing file must not lose a call.
#[test]
fn no_file_or_an_unreadable_one_is_a_fault_and_records() {
    let empty = tempfile::tempdir().unwrap();
    assert!(records(empty.path(), Utc::now()));
    assert_eq!(window(empty.path()), None);
    let garbage = root_with("tomorrow-ish");
    assert!(records(garbage.path(), Utc::now()));
}

// --- writing it (the break-glass half) --------------------------------------

#[test]
fn a_written_window_reads_back() {
    let dir = tempfile::tempdir().expect("tmp");
    let until = at("2026-10-23T11:00:00Z");
    for written in [Window::Off, Window::Record(until), Window::Hold(until)] {
        audiod::gate::write(dir.path(), written).expect("write");
        assert_eq!(window(dir.path()), Some(written));
    }
}

#[test]
fn record_opens_a_capped_window_and_stop_closes_it() {
    let dir = tempfile::tempdir().expect("tmp");
    let now = at("2026-10-23T10:00:00Z");
    let opened = audiod::gate::record(dir.path(), now, 60 * 24 * 7).expect("write");
    assert_eq!(opened, Window::Record(now + Duration::hours(24)));
    assert!(records(dir.path(), now));
    audiod::gate::write(dir.path(), Window::Off).expect("stop");
    assert!(!records(dir.path(), now));
}

#[test]
fn pause_and_unpause_keep_the_end() {
    let dir = root_with("record 2026-10-23T11:00:00Z");
    let now = at("2026-10-23T10:00:00Z");
    let until = at("2026-10-23T11:00:00Z");
    assert_eq!(
        audiod::gate::change(dir.path(), now, Window::paused).expect("pause"),
        Window::Hold(until)
    );
    assert!(!records(dir.path(), now));
    assert_eq!(
        audiod::gate::change(dir.path(), now, Window::unpaused).expect("unpause"),
        Window::Record(until)
    );
    assert!(records(dir.path(), now));
}

/// Pausing a broken file writes `off`, not a fault: the person asked for quiet.
#[test]
fn pausing_a_fault_stops_it() {
    let dir = tempfile::tempdir().expect("tmp");
    let now = at("2026-10-23T10:00:00Z");
    assert_eq!(
        audiod::gate::change(dir.path(), now, Window::paused).expect("pause"),
        Window::Off
    );
    assert!(!records(dir.path(), now));
}
