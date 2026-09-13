use audiod::pause::{PAUSE_FILE, is_paused};
use chrono::Utc;

fn root_with(content: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(PAUSE_FILE), content).unwrap();
    dir
}

#[test]
fn no_file_means_recording() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!is_paused(dir.path(), Utc::now()));
}

#[test]
fn a_future_aware_timestamp_pauses() {
    let dir = root_with("2030-01-01T00:00:00+00:00");
    assert!(is_paused(dir.path(), Utc::now()));
}

#[test]
fn a_past_timestamp_means_the_pause_expired() {
    let dir = root_with("2020-01-01T00:00:00+00:00");
    assert!(!is_paused(dir.path(), Utc::now()));
}

#[test]
fn a_naive_timestamp_reads_as_utc() {
    let dir = root_with("2030-01-01T00:00:00.500000");
    assert!(is_paused(dir.path(), Utc::now()));
}

#[test]
fn garbage_means_recording_not_a_crash() {
    let dir = root_with("tomorrow-ish");
    assert!(!is_paused(dir.path(), Utc::now()));
}
