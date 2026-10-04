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
fn files_stored_before_the_clips_table_get_clips_when_it_appears() {
    let dir = tempfile::tempdir().expect("tmp");
    {
        let old = Connection::open(dir.path().join("ingest.sqlite")).expect("old db");
        old.execute_batch(
            "CREATE TABLE segments (filename TEXT PRIMARY KEY, source TEXT NOT NULL,
                 start_utc TEXT NOT NULL, bytes INTEGER NOT NULL, sha256 TEXT NOT NULL,
                 received_utc TEXT NOT NULL, sent_utc TEXT);
             INSERT INTO segments VALUES
                 ('usb-20260905T120100.flac', 'usb', '2026-09-05T12:01:00Z', 1, 'x', 'r', NULL),
                 ('usb-20260905T120000.flac', 'usb', '2026-09-05T12:00:00Z', 1, 'x', 'r', NULL);",
        )
        .expect("old rows");
    }
    let conn = store::open(dir.path()).expect("open");

    let first = recalld::clips::by_filename(&conn, "usb-20260905T120000.flac")
        .unwrap()
        .unwrap();
    let second = recalld::clips::by_filename(&conn, "usb-20260905T120100.flac")
        .unwrap()
        .unwrap();
    assert!(first.id < second.id, "backfilled in start order");
    let count: i64 = conn
        .query_row("SELECT count(*) FROM clips", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2);
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

#[test]
fn uploads_stored_outside_ingest_are_adopted_once() {
    let (dir, ingest) = ingest();
    let meaning = recalld::work::open_write(dir.path()).expect("meaning");
    recalld::meaning_schema::ensure(&meaning).expect("schema");
    let path = dir
        .path()
        .join("meeting-20260520-1901/meeting-20260520-1901-20260520T180121.mp3");
    meaning
        .execute_batch(&format!(
            "INSERT INTO sources (id, name, kind) VALUES ('meeting-20260520-1901', 'm', 'upload');
             INSERT INTO audio_segments (source_id, path, start_utc, end_utc, sample_rate, channels)
             VALUES ('meeting-20260520-1901', '{}', '2026-05-20T18:01:21+00:00',
                     '2026-05-20T18:30:00+00:00', 16000, 1);",
            path.display()
        ))
        .expect("legacy upload");

    assert_eq!(
        recalld::clips::adopt_outside_ingest(&meaning, &ingest, dir.path()).unwrap(),
        1
    );
    assert_eq!(
        recalld::clips::adopt_outside_ingest(&meaning, &ingest, dir.path()).unwrap(),
        0
    );
    let clip = recalld::clips::for_audio_path(&ingest, dir.path(), &path.to_string_lossy())
        .unwrap()
        .expect("found by its audio path");
    assert_eq!(
        clip.path,
        "meeting-20260520-1901/meeting-20260520-1901-20260520T180121.mp3"
    );
    assert_eq!(clip.start, Instant::parse("2026-05-20T18:01:21Z").unwrap());
}
