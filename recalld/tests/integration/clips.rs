//! Clip identity (#1911): every stored file has exactly one clip, the file is
//! the key, and a rename keeps the id.

use audiocore::instant::IngestStamp;
use recalld::store::{self, Row};
use rusqlite::Connection;
use transcript::Instant;

fn row(source: &str, filename: &str, start: &str) -> Row {
    Row {
        source: source.into(),
        filename: filename.into(),
        start_utc: IngestStamp::parse(start).expect("instant"),
        bytes: 1,
        sha256: "x".into(),
        received_utc: IngestStamp::parse(start).expect("instant"),
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
fn a_start_that_is_not_an_instant_cannot_become_a_row() {
    // A clip without a time must not be stored silently: the row's type
    // cannot hold one.
    assert_eq!(IngestStamp::parse("not a time"), None);
}
