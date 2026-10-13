//! Clip identity (#1911): every stored file has exactly one clip, the file is
//! the key, and a rename keeps the id.

use recalld::store::{self, Row};
use rusqlite::Connection;
use transcript::Instant;

fn row(source: &str, filename: &str, start: &str) -> Row {
    Row {
        source: source.into(),
        filename: filename.into(),
        start_utc: start.into(),
        bytes: 1,
        sha256: "x".into(),
        received_utc: start.into(),
        sent_utc: None,
    }
}

fn ingest() -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().expect("tmp");
    let conn = store::open(dir.path()).expect("open");
    (dir, conn)
}

#[test]
fn storing_a_file_gives_it_a_clip() {
    let (_dir, conn) = ingest();
    store::insert(
        &conn,
        &row(
            "pixel5",
            "pixel5-20261004T143001.phone.flac",
            "2026-10-04T14:30:01Z",
        ),
    )
    .expect("insert");

    let clip = recalld::clips::by_filename(&conn, "pixel5-20261004T143001.phone.flac")
        .expect("read")
        .expect("a clip");
    assert_eq!(clip.source.as_str(), "pixel5");
    assert_eq!(clip.start, Instant::parse("2026-10-04T14:30:01Z").unwrap());
    assert_eq!(clip.path, "ingest/pixel5/pixel5-20261004T143001.phone.flac");
}

#[test]
fn two_files_of_one_source_and_second_are_two_clips() {
    // The Mac's cut of a phone's stream and the phone's own copy.
    let (_dir, conn) = ingest();
    store::insert(
        &conn,
        &row(
            "pixel5",
            "pixel5-20261004T143001.flac",
            "2026-10-04T14:30:01Z",
        ),
    )
    .expect("mac");
    store::insert(
        &conn,
        &row(
            "pixel5",
            "pixel5-20261004T143001.phone.flac",
            "2026-10-04T14:30:01Z",
        ),
    )
    .expect("phone");

    let mac = recalld::clips::by_filename(&conn, "pixel5-20261004T143001.flac")
        .unwrap()
        .unwrap();
    let phone = recalld::clips::by_filename(&conn, "pixel5-20261004T143001.phone.flac")
        .unwrap()
        .unwrap();
    assert_ne!(mac.id, phone.id);
    assert_eq!(mac.start, phone.start);
}

#[test]
fn a_start_that_will_not_convert_fails_the_files_own_insert() {
    let (_dir, conn) = ingest();
    let refused = store::insert(&conn, &row("usb", "usb-x.flac", "not a time"));

    assert!(
        refused.is_err(),
        "a clip without a time must not be stored silently"
    );
    assert!(store::lookup(&conn, "usb-x.flac").unwrap().is_none());
}

#[test]
fn a_renamed_file_keeps_its_clip() {
    let (_dir, conn) = ingest();
    store::insert(
        &conn,
        &row(
            "pixel5",
            "pixel5-20260920T120000.wav",
            "2026-09-20T12:00:00Z",
        ),
    )
    .expect("wav");
    let before = recalld::clips::by_filename(&conn, "pixel5-20260920T120000.wav")
        .unwrap()
        .unwrap();

    recalld::clips::rename(
        &conn,
        "pixel5-20260920T120000.wav",
        "pixel5-20260920T120000.phone.flac",
    )
    .expect("rename");
    store::insert(
        &conn,
        &row(
            "pixel5",
            "pixel5-20260920T120000.phone.flac",
            "2026-09-20T12:00:00Z",
        ),
    )
    .expect("flac");

    let after = recalld::clips::by_filename(&conn, "pixel5-20260920T120000.phone.flac")
        .unwrap()
        .unwrap();
    assert_eq!(after.id, before.id);
    assert_eq!(
        after.path,
        "ingest/pixel5/pixel5-20260920T120000.phone.flac"
    );
    let count: i64 = conn
        .query_row("SELECT count(*) FROM clips", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}
