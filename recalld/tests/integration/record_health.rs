//! What the doctor asks the record about: requests that failed on our side,
//! and minutes a mic has shown twice (`recalld::record_health`).

use chrono::{DateTime, Utc};
use recalld::record_health::{doubled_minutes, fault_log, faults_since, measure};

fn at(raw: &str) -> DateTime<Utc> {
    raw.parse().expect("an instant")
}

#[test]
fn faults_are_counted_from_the_window_start_and_the_newest_is_named() {
    let dir = tempfile::tempdir().expect("tmp");
    let log = dir.path().join("faults.jsonl");
    std::fs::write(
        &log,
        concat!(
            r#"{"utc":"2026-09-26T20:05:57Z","what":"correct","error":"database is locked"}"#,
            "\n",
            r#"{"utc":"2026-09-28T15:25:53Z","what":"correct","error":"database is locked"}"#,
            "\n",
            "a line that is not json\n",
            r#"{"utc":"2026-09-28T15:30:00Z","what":"no-speech","error":"disk I/O error"}"#,
            "\n",
        ),
    )
    .expect("log");

    let faults = faults_since(&log, at("2026-09-28T00:00:00Z")).expect("read");

    assert_eq!(faults.count, 2);
    let last = faults.last.expect("the newest");
    assert_eq!(last.what, "no-speech");
    assert_eq!(last.error, "disk I/O error");
}

#[test]
fn no_fault_log_yet_is_no_faults() {
    let dir = tempfile::tempdir().expect("tmp");

    let faults =
        faults_since(&dir.path().join("faults.jsonl"), at("2026-09-28T00:00:00Z")).expect("read");

    assert_eq!(faults.count, 0);
    assert!(faults.last.is_none());
}

/// Only this test sets the process's fault log: it is set once, at startup.
#[test]
fn a_fault_answered_as_500_is_kept_in_the_fault_log() {
    let dir = tempfile::tempdir().expect("tmp");
    let log = dir.path().join("faults.jsonl");
    recalld::route::keep_faults_in(log.clone());

    let _ = recalld::route::faulted("record health test", &"it broke");

    let faults = faults_since(&log, at("2026-01-01T00:00:00Z")).expect("read");
    let last = faults.last.expect("kept");
    assert_eq!(last.what, "record health test");
    assert_eq!(last.error, "it broke");
}

fn meaning() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().expect("mem");
    recalld::meaning_schema::ensure(&conn).expect("schema");
    conn.execute_batch(
        "INSERT INTO sources (id, name, kind) VALUES ('pixel5', 'pixel5', 'tcp_pcm');",
    )
    .expect("source");
    conn
}

/// A clip of `pixel5` a minute long, with `lines` visible lines.
fn clip(conn: &rusqlite::Connection, start: &str, ext: &str, lines: usize) {
    let start = at(start);
    let end = start + chrono::Duration::seconds(60);
    let iso = |t: DateTime<Utc>| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
    conn.execute(
        "INSERT INTO audio_segments (source_id, path, start_utc, end_utc, sample_rate, channels)
         VALUES ('pixel5', ?1, ?2, ?3, 16000, 1)",
        (
            format!("/data/pixel5/{}.{ext}", start.timestamp()),
            iso(start),
            iso(end),
        ),
    )
    .expect("clip");
    let id = conn.last_insert_rowid();
    for _ in 0..lines {
        conn.execute(
            "INSERT INTO transcript_segments (audio_segment_id, start_utc, end_utc, text, asr_model)
             VALUES (?1, ?2, ?2, 'words', 'whisper')",
            (id, iso(start)),
        )
        .expect("line");
    }
}

#[test]
fn a_minute_shown_from_two_copies_is_counted() {
    let conn = meaning();
    clip(&conn, "2026-09-28T13:16:15Z", "flac", 3);
    clip(&conn, "2026-09-28T13:16:16Z", "wav", 3);

    let doubled = doubled_minutes(&conn, at("2026-09-28T00:00:00Z")).expect("count");

    assert_eq!(doubled.count, 1);
    let last = doubled.last.expect("named");
    assert_eq!(last.source, "pixel5");
}

#[test]
fn a_copy_whose_lines_are_hidden_is_not_doubled() {
    let conn = meaning();
    clip(&conn, "2026-09-28T13:16:15Z", "flac", 3);
    clip(&conn, "2026-09-28T13:16:16Z", "wav", 0);

    assert_eq!(
        doubled_minutes(&conn, at("2026-09-28T00:00:00Z"))
            .expect("count")
            .count,
        0
    );
}

#[test]
fn a_mics_next_minute_and_an_old_double_are_not_counted() {
    let conn = meaning();
    clip(&conn, "2026-09-28T13:16:15Z", "flac", 3);
    clip(&conn, "2026-09-28T13:17:15Z", "flac", 3);
    clip(&conn, "2026-09-20T10:00:00Z", "flac", 3);
    clip(&conn, "2026-09-20T10:00:01Z", "wav", 3);

    assert_eq!(
        doubled_minutes(&conn, at("2026-09-28T00:00:00Z"))
            .expect("count")
            .count,
        0
    );
}

/// An unreadable log is reported as a fault, not as a route that fails: the
/// doctor then says what is wrong instead of skipping.
#[test]
fn a_fault_log_that_cannot_be_read_is_itself_a_fault() {
    let dir = tempfile::tempdir().expect("tmp");
    let conn = recalld::work::open_write(dir.path()).expect("recall db");
    recalld::meaning_schema::ensure(&conn).expect("schema");
    drop(conn);
    // A directory where the log should be: reading it fails.
    std::fs::create_dir_all(fault_log(dir.path())).expect("a directory in the way");

    let health = measure(dir.path(), at("2026-09-28T00:00:00Z")).expect("measured");

    assert_eq!(health.faults.count, 1);
    assert_eq!(
        health.faults.last.expect("named").what,
        "reading the fault log"
    );
}
