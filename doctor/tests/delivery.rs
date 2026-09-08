//! Store-and-forward: is the fleet's copy keeping up, and did anything collide?
//!
//! Driven through `delivery_checks` against a real archive layout and a real
//! `upload-state.sqlite`, because the grammar and the state read are the two
//! halves that have to agree — testing either alone would pass while the pair
//! disagreed about which files count.

use chrono::{Duration, Utc};
use doctor::check::Verdict;
use doctor::delivery::delivery_checks;
use std::path::Path;

/// audiod's uploader state: what it verified, and what came back 409.
fn uploader_state(root: &Path, uploads: &[&str], conflicts: &[&str]) {
    let conn = rusqlite::Connection::open(root.join("upload-state.sqlite")).unwrap();
    conn.execute_batch(
        "CREATE TABLE uploads (filename TEXT PRIMARY KEY);
         CREATE TABLE conflicts (filename TEXT PRIMARY KEY);",
    )
    .unwrap();
    for name in uploads {
        conn.execute("INSERT INTO uploads VALUES (?1)", [name])
            .unwrap();
    }
    for name in conflicts {
        conn.execute("INSERT INTO conflicts VALUES (?1)", [name])
            .unwrap();
    }
}

/// A segment file, aged by setting its mtime `minutes` into the past.
fn segment(root: &Path, source: &str, name: &str, minutes: i64) {
    let dir = root.join(source);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, b"not really audio").unwrap();
    let when = std::time::SystemTime::now() - std::time::Duration::from_secs((minutes * 60) as u64);
    filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(when)).unwrap();
}

fn find<'a>(checks: &'a [doctor::check::Check], label: &str) -> &'a doctor::check::Check {
    checks.iter().find(|c| c.label == label).expect(label)
}

#[test]
fn no_uploader_state_reports_nothing_at_all() {
    // Stage B is not deployed everywhere; a stock machine has no mirror to be
    // behind on and must not grow a red check saying so.
    let dir = tempfile::tempdir().unwrap();
    assert!(delivery_checks(dir.path(), Utc::now()).is_empty());
}

#[test]
fn a_delivered_segment_is_not_a_backlog() {
    let dir = tempfile::tempdir().unwrap();
    segment(dir.path(), "usb", "usb-20260908T191122.flac", 120);
    uploader_state(dir.path(), &["usb-20260908T191122.flac"], &[]);
    let checks = delivery_checks(dir.path(), Utc::now());
    let complete = find(&checks, "delivery complete");
    assert_eq!(complete.verdict, Verdict::Pass);
    assert_eq!(complete.value, Some(0.0));
}

#[test]
fn the_backlog_is_graded_by_its_oldest_member() {
    // Deliveries run oldest-first, so the oldest is how far behind the mirror
    // actually is. 30 min warns, 6 h fails.
    let dir = tempfile::tempdir().unwrap();
    segment(dir.path(), "usb", "usb-20260908T191122.flac", 45);
    segment(dir.path(), "usb", "usb-20260908T192122.flac", 10);
    uploader_state(dir.path(), &[], &[]);
    let checks = delivery_checks(dir.path(), Utc::now());
    let complete = find(&checks, "delivery complete");
    assert_eq!(complete.verdict, Verdict::Warn);
    assert_eq!(complete.value, Some(2.0));

    let dir = tempfile::tempdir().unwrap();
    segment(dir.path(), "usb", "usb-20260908T191122.flac", 7 * 60);
    uploader_state(dir.path(), &[], &[]);
    assert_eq!(
        find(
            &delivery_checks(dir.path(), Utc::now()),
            "delivery complete"
        )
        .verdict,
        Verdict::Fail
    );
}

#[test]
fn the_segment_ffmpeg_may_still_be_writing_is_not_undelivered() {
    // The newest file in a source directory is inside the open grace: it is not
    // late, it is unfinished. Without this every healthy pass reports a backlog
    // of one, for ever.
    let dir = tempfile::tempdir().unwrap();
    segment(dir.path(), "usb", "usb-20260908T191122.flac", 0);
    uploader_state(dir.path(), &[], &[]);
    let checks = delivery_checks(dir.path(), Utc::now());
    assert_eq!(find(&checks, "delivery complete").value, Some(0.0));
}

#[test]
fn only_files_matching_the_delivery_grammar_are_counted() {
    // The grammar must stay the subset audiod ships and recalld's parser
    // accepts. A `.alive` marker and a stray note are not undelivered audio.
    let dir = tempfile::tempdir().unwrap();
    let usb = dir.path().join("usb");
    std::fs::create_dir_all(&usb).unwrap();
    for name in [
        ".alive",
        "notes.txt",
        "usb-20260908T191122.mp3",
        "pixel9-20260908T191122.flac",
        "usb-2026-09-08T191122.flac",
    ] {
        std::fs::write(usb.join(name), b"x").unwrap();
    }
    segment(dir.path(), "usb", "usb-20260908T191122.flac", 45);
    segment(dir.path(), "usb", "usb-20260908T192122.flac", 44);
    uploader_state(dir.path(), &[], &[]);
    let checks = delivery_checks(dir.path(), Utc::now());
    assert_eq!(
        find(&checks, "delivery complete").value,
        Some(2.0),
        "only the two grammar-matching files are audio awaiting delivery"
    );
}

#[test]
fn a_conflict_warns_without_failing_and_names_a_few() {
    // Nothing was lost: Isis holds different bytes under a name we also hold,
    // and a person has to look.
    let dir = tempfile::tempdir().unwrap();
    uploader_state(
        dir.path(),
        &[],
        &["a-20260908T191122.flac", "b-20260908T191122.flac"],
    );
    let checks = delivery_checks(dir.path(), Utc::now());
    let conflicts = find(&checks, "no delivery conflicts");
    assert_eq!(conflicts.verdict, Verdict::Warn);
    assert_eq!(conflicts.value, Some(2.0));
    assert!(conflicts.observed.contains("a-20260908T191122.flac"));
}

#[test]
fn a_conflicted_name_does_not_also_read_as_undelivered() {
    // It was handled — badly, and that is the other check's business.
    let dir = tempfile::tempdir().unwrap();
    segment(dir.path(), "usb", "usb-20260908T191122.flac", 7 * 60);
    uploader_state(dir.path(), &[], &["usb-20260908T191122.flac"]);
    let checks = delivery_checks(dir.path(), Utc::now());
    assert_eq!(find(&checks, "delivery complete").verdict, Verdict::Pass);
    assert_eq!(
        find(&checks, "no delivery conflicts").verdict,
        Verdict::Warn
    );
}

#[test]
fn the_open_grace_is_shorter_than_the_warn_line() {
    // Otherwise a segment could age out of the grace straight into a warning.
    assert!(Duration::minutes(3) < Duration::minutes(30));
}
