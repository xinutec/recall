//! The deletion guard's own guards.

use audiocore::vad::UNKNOWN_SECONDS;
use chrono::{Duration, Utc};
use rusqlite::Connection;

fn archive(rows: &[(i64, &str, &str)]) -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().expect("tmp");
    let conn = Connection::open(dir.path().join("recall.sqlite")).expect("db");
    conn.execute_batch(
        "CREATE TABLE audio_segments (
             id INTEGER PRIMARY KEY, source_id TEXT, path TEXT,
             start_utc TEXT, end_utc TEXT, speech_s REAL);",
    )
    .expect("schema");
    for (id, path, end) in rows {
        conn.execute(
            "INSERT INTO audio_segments (id, source_id, path, start_utc, end_utc, speech_s)
             VALUES (?1, 'usb', ?2, ?3, ?3, NULL)",
            rusqlite::params![id, path, end],
        )
        .expect("insert");
    }
    (dir, conn)
}

#[test]
fn a_segment_still_being_written_is_left_alone() {
    // ⚠ The bug this was written for, and it damaged real data before it was
    // caught: the first run took the five NEWEST segments, which ffmpeg had not
    // finished, failed to decode every one, and stamped them unlistened. A row
    // with a value is never revisited, so that is permanent — and the files were
    // 185 KB of ordinary speech a minute later.
    let now = Utc::now();
    let fresh = now.to_rfc3339();
    let (dir, conn) = archive(&[(1, "/nope/fresh.opus", &fresh)]);
    drop(conn);

    let pass = audiod::speech_scan::run(dir.path(), 10).expect("pass");
    assert_eq!(pass.measured, 0);
    assert_eq!(
        pass.unreadable, 0,
        "an unfinished segment must be SKIPPED, not recorded as unlistened"
    );
    assert_eq!(
        audiod::speech_scan::remaining(dir.path()).unwrap(),
        1,
        "and it must stay unmeasured, so a later pass can do it properly"
    );
}

#[test]
fn a_pass_where_everything_failed_writes_nothing() {
    // ⚠ The one that cost real rows, twice in ten minutes. Both times the cause
    // was this process — ffmpeg mid-write, then ffmpeg absent from PATH — and
    // both times a verdict was written that is never revisited. A broken file
    // among good ones is believable; EVERY file broken is the instrument.
    let old = (Utc::now() - Duration::hours(1)).to_rfc3339();
    let (dir, conn) = archive(&[
        (1, "/definitely/not/audio.opus", &old),
        (2, "/also/not/audio.opus", &old),
        (3, "/nor/this.opus", &old),
    ]);
    drop(conn);

    let err = audiod::speech_scan::run(dir.path(), 10).expect_err("must refuse");
    assert!(
        err.to_string().contains("ffmpeg"),
        "it names the likely cause: {err}"
    );
    assert_eq!(
        audiod::speech_scan::remaining(dir.path()).unwrap(),
        3,
        "every row must be left unmeasured, so a working pass can still do them"
    );
}

#[test]
fn a_closed_segment_that_cannot_be_decoded_is_recorded_as_unlistened() {
    // The other half: a genuinely broken file must NOT be skipped for ever, and
    // must NOT be recorded as 0.0. "We could not look" and "nobody spoke" are
    // different answers, and only one of them licenses deleting the audio.
    let old = (Utc::now() - Duration::hours(1)).to_rfc3339();
    let (dir, conn) = archive(&[(1, "/definitely/not/audio.opus", &old)]);
    drop(conn);

    let pass = audiod::speech_scan::run(dir.path(), 10).expect("pass");
    assert_eq!(pass.unreadable, 1);
    assert_eq!(audiod::speech_scan::remaining(dir.path()).unwrap(), 0);

    let conn = Connection::open(dir.path().join("recall.sqlite")).unwrap();
    let stored: f64 = conn
        .query_row("SELECT speech_s FROM audio_segments WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    // An exact compare is right here and clippy's default is not: UNKNOWN_SECONDS
    // is a SENTINEL written verbatim, never a computed quantity approached from
    // somewhere. Compared with a tolerance so the lint stays on for the places
    // where it IS a real hazard.
    assert!(
        (stored - UNKNOWN_SECONDS).abs() < f64::EPSILON,
        "the sentinel must be stored exactly, got {stored}"
    );
    assert!(
        stored < 0.0,
        "never 0.0 — that would read as 'nobody spoke'"
    );
}

#[test]
fn an_empty_archive_is_not_an_error() {
    let (dir, conn) = archive(&[]);
    drop(conn);
    let pass = audiod::speech_scan::run(dir.path(), 10).expect("pass");
    assert_eq!(pass.measured, 0);
    assert_eq!(pass.unreadable, 0);
}
