use crate::common;

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
fn a_database_that_refuses_is_swallowed_not_fatal() {
    // ⚠ A file that is PRESENT and unusable is the case this still covers, and
    // it is a different one from a machine that has no meaning plane at all
    // (below). Here something is wrong and the error line is wanted; there,
    // nothing is wrong and it is not. Both must return without panicking —
    // bookkeeping must never take the audio pump down with it.
    let dir = tempfile::tempdir().unwrap();
    rusqlite::Connection::open(dir.path().join("recall.sqlite"))
        .unwrap()
        .execute_batch("CREATE TABLE unrelated (x INTEGER);")
        .unwrap();

    register_source(dir.path(), "pixel9");
    add_capture_event(dir.path(), KIND_INGEST_CONNECT, Utc::now(), "pixel9", None);
}

#[test]
fn a_recorder_with_no_meaning_plane_writes_nothing_and_reports_no_fault() {
    // ⚠ geb is store-and-forward: it has no `recall.sqlite` and is not supposed
    // to (docs/architecture.md C3). Both writes used to try anyway and log an
    // ERROR on every start of a correctly configured recorder, which is how a
    // log gets ignored (#1566).
    //
    // ⚠ And neither may CREATE the file. A local meaning plane the fleet would
    // have to merge is the thing store-and-forward exists to avoid — so the
    // assertion that matters is that the directory is still empty afterwards.
    let dir = tempfile::tempdir().unwrap();
    assert!(!audiod::store::has_meaning_plane(dir.path()));

    register_source(dir.path(), "geb");
    add_capture_event(
        dir.path(),
        KIND_INGEST_CONNECT,
        Utc.with_ymd_and_hms(2026, 9, 12, 15, 9, 13).unwrap(),
        "geb",
        None,
    );
    let pass = audiod::register::run(dir.path(), 10).expect("no plane, no work, no error");

    assert_eq!(pass, audiod::register::Pass::default());
    assert!(
        !dir.path().join("recall.sqlite").exists(),
        "a recorder must not mint an archive of its own"
    );
}
