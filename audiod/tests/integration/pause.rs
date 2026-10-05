use audiod::pause::{PAUSE_FILE, is_paused};
use chrono::{DateTime, Utc};

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

// --- writing the pause (the break-glass half) ---------------------------------

#[test]
fn a_pause_is_written_in_the_spelling_every_reader_already_parses() {
    // An unparseable pause file means recording: it must round-trip.
    let dir = tempfile::tempdir().expect("tmp");
    let now: DateTime<Utc> = "2026-09-17T14:00:00+00:00".parse().expect("t");

    let until = audiod::pause::pause(dir.path(), now, Some(30)).expect("write");

    let expected: DateTime<Utc> = "2026-09-17T14:30:00+00:00".parse().expect("t");
    assert_eq!(until, expected);
    assert_eq!(audiod::pause::paused_until(dir.path()), Some(until));
    assert!(audiod::pause::is_paused(dir.path(), now));
}

#[test]
fn a_pause_is_capped_so_a_forgotten_one_cannot_silence_the_house_for_ever() {
    // Capped at 24 h.
    let now: DateTime<Utc> = "2026-09-17T14:00:00+00:00".parse().expect("t");

    let capped = audiod::pause::resume_by(now, Some(60 * 24 * 7));
    let default = audiod::pause::resume_by(now, None);

    assert_eq!(capped, default, "a week must clamp to the cap");
    let cap: DateTime<Utc> = "2026-09-18T14:00:00+00:00".parse().expect("t");
    assert_eq!(capped, cap);
    // A negative span does not put the resume time in the past.
    assert_eq!(audiod::pause::resume_by(now, Some(-5)), now);
}

#[test]
fn resuming_is_the_end_state_not_the_write_so_no_pause_is_success() {
    // Resume is idempotent.
    let dir = tempfile::tempdir().expect("tmp");
    audiod::pause::resume(dir.path()).expect("no file is already resumed");

    let now: DateTime<Utc> = "2026-09-17T14:00:00+00:00".parse().expect("t");
    audiod::pause::pause(dir.path(), now, Some(30)).expect("write");
    audiod::pause::resume(dir.path()).expect("clear");

    assert_eq!(audiod::pause::paused_until(dir.path()), None);
    assert!(!audiod::pause::is_paused(dir.path(), now));
}
