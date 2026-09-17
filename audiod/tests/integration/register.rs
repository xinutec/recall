//! The coverage record the loss alarm compares against (#1650).

use chrono::{Duration, Utc};
use rusqlite::Connection;

/// A meaning plane with one registered device and no segments.
fn archive() -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().expect("tmp");
    let conn = Connection::open(dir.path().join("recall.sqlite")).expect("db");
    conn.execute_batch(
        "CREATE TABLE sources (id TEXT PRIMARY KEY, name TEXT, kind TEXT NOT NULL);
         CREATE TABLE audio_segments (
             id INTEGER PRIMARY KEY, source_id TEXT NOT NULL, path TEXT NOT NULL,
             start_utc TEXT NOT NULL, end_utc TEXT NOT NULL,
             sample_rate INTEGER NOT NULL, channels INTEGER NOT NULL,
             UNIQUE (source_id, start_utc));
         INSERT INTO sources (id, name, kind) VALUES
             ('usb', 'usb', 'coreaudio'),
             ('meeting-20260907-0905', 'Meeting', 'upload');",
    )
    .expect("schema");
    std::fs::create_dir_all(dir.path().join("usb")).expect("dir");
    (dir, conn)
}

/// A segment file that exists but holds nothing decodable — enough to be found.
fn clip(dir: &std::path::Path, source: &str, stamp: &str) {
    std::fs::create_dir_all(dir.join(source)).expect("dir");
    std::fs::write(
        dir.join(source).join(format!("{source}-{stamp}.flac")),
        b"x",
    )
    .expect("clip");
}

#[test]
fn a_closed_clip_with_no_row_is_offered() {
    let (dir, conn) = archive();
    clip(dir.path(), "usb", "20260910T100000");

    let work = audiod::register::unregistered(
        &conn,
        dir.path(),
        "2026-09-10T12:00:00Z".parse().expect("t"),
        10,
    )
    .expect("scan");
    assert_eq!(work.len(), 1);
    assert_eq!(work[0].source, "usb");
    // ⚠ The MEANING plane's spelling. `known` is read from the same column and
    // the two are compared as TEXT, so a trailing Z here would re-register every
    // clip on every pass, for ever.
    assert_eq!(work[0].start_utc, "2026-09-10T10:00:00.000000+00:00");
}

#[test]
fn a_clip_still_being_written_is_left_alone() {
    // ⚠ Same three-minute grace as the speech scanner, and for the harder
    // reason: this pass writes an END time. Stamping one onto a file ffmpeg is
    // still appending to records a clip as shorter than it was, and the row is
    // never revisited — so the loss check would see a gap that never existed.
    let (dir, conn) = archive();
    let now = Utc::now();
    let stamp = (now - Duration::seconds(30))
        .format("%Y%m%dT%H%M%S")
        .to_string();
    clip(dir.path(), "usb", &stamp);

    let work = audiod::register::unregistered(&conn, dir.path(), now, 10).expect("scan");
    assert!(work.is_empty(), "got {work:?}");
}

#[test]
fn a_clip_already_registered_is_not_offered_again() {
    let (dir, conn) = archive();
    clip(dir.path(), "usb", "20260910T100000");
    conn.execute(
        "INSERT INTO audio_segments
             (source_id, path, start_utc, end_utc, sample_rate, channels)
         VALUES ('usb', '/x', '2026-09-10T10:00:00.000000+00:00',
                 '2026-09-10T10:01:00.000000+00:00', 48000, 1)",
        [],
    )
    .expect("row");

    let work = audiod::register::unregistered(
        &conn,
        dir.path(),
        "2026-09-10T12:00:00Z".parse().expect("t"),
        10,
    )
    .expect("scan");
    assert!(work.is_empty(), "got {work:?}");
}

#[test]
fn an_uploaded_session_is_not_this_passs_business() {
    // ⚠ An upload is a source with no recorder that could stop or lose speech,
    // and its rows are written where it arrives. Registering it here would put a
    // second row on one clip and make the loss check reason about a meeting.
    let (dir, conn) = archive();
    clip(dir.path(), "meeting-20260907-0905", "20260907T080526");

    let work = audiod::register::unregistered(
        &conn,
        dir.path(),
        "2026-09-10T12:00:00Z".parse().expect("t"),
        10,
    )
    .expect("scan");
    assert!(work.is_empty(), "got {work:?}");
}

#[test]
fn the_oldest_gap_is_filled_first_and_the_pass_is_bounded() {
    // ⚠ OLDEST first, unlike the speech scanner. That pass answers "what was
    // said recently"; this one fills a hole in a continuous record, and a
    // coverage table with holes in the middle is what the loss check reports.
    let (dir, conn) = archive();
    for stamp in ["20260910T100000", "20260910T100100", "20260910T100200"] {
        clip(dir.path(), "usb", stamp);
    }

    let work = audiod::register::unregistered(
        &conn,
        dir.path(),
        "2026-09-10T12:00:00Z".parse().expect("t"),
        2,
    )
    .expect("scan");
    let starts: Vec<&str> = work.iter().map(|u| u.start_utc.as_str()).collect();
    assert_eq!(
        starts,
        [
            "2026-09-10T10:00:00.000000+00:00",
            "2026-09-10T10:01:00.000000+00:00"
        ]
    );
}

#[test]
fn a_pass_where_nothing_reads_refuses_to_record_that() {
    // ⚠ Every file failing is ffmpeg missing or the volume unmounted, not a
    // house full of broken recordings. The speech scanner learned this by
    // damaging real rows twice in ten minutes; this pass writes rows that are
    // never revisited either.
    let (dir, conn) = archive();
    for stamp in ["20260910T100000", "20260910T100100"] {
        clip(dir.path(), "usb", stamp); // one byte of "x" — not audio
    }
    drop(conn);

    let err = audiod::register::run(dir.path(), 10).expect_err("must refuse");
    assert!(err.to_string().contains("ffmpeg"), "got {err}");
}
