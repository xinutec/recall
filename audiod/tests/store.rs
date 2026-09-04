mod common;

use audiod::store::{KIND_INGEST_CONNECT, add_capture_event, register_source};
use chrono::{TimeZone, Utc};
use std::path::Path;

fn query_one<T: rusqlite::types::FromSql>(root: &Path, sql: &str) -> T {
    rusqlite::Connection::open(root.join("recall.sqlite"))
        .unwrap()
        .query_row(sql, [], |r| r.get(0))
        .unwrap()
}

#[test]
fn registration_is_idempotent_and_keeps_a_user_chosen_name() {
    let dir = tempfile::tempdir().unwrap();
    common::create_schema(dir.path());
    register_source(dir.path(), "pixel9");
    register_source(dir.path(), "pixel9");
    let kind: String = query_one(dir.path(), "SELECT kind FROM sources WHERE id = 'pixel9'");
    assert_eq!(kind, "tcp_pcm");
    // A name chosen in the UI survives re-registration.
    rusqlite::Connection::open(dir.path().join("recall.sqlite"))
        .unwrap()
        .execute(
            "UPDATE sources SET name = 'Kitchen phone' WHERE id = 'pixel9'",
            [],
        )
        .unwrap();
    register_source(dir.path(), "pixel9");
    let name: String = query_one(dir.path(), "SELECT name FROM sources WHERE id = 'pixel9'");
    assert_eq!(name, "Kitchen phone");
}

#[test]
fn events_land_with_a_python_parsable_timestamp() {
    let dir = tempfile::tempdir().unwrap();
    common::create_schema(dir.path());
    let utc = Utc.with_ymd_and_hms(2026, 9, 4, 19, 6, 1).unwrap();
    add_capture_event(dir.path(), KIND_INGEST_CONNECT, utc, "pixel9", None);
    // datetime.fromisoformat must accept this — the loss reconciler reads it.
    let stored: String = query_one(dir.path(), "SELECT utc FROM capture_events");
    assert_eq!(stored, "2026-09-04T19:06:01.000000+00:00");
}

#[test]
fn a_missing_database_is_swallowed_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    // No schema, no file: both writes must return without panicking —
    // bookkeeping must never take the audio pump down with it.
    register_source(dir.path(), "pixel9");
    add_capture_event(dir.path(), KIND_INGEST_CONNECT, Utc::now(), "pixel9", None);
}
