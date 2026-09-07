//! Vocabulary and refine (stage F1) — recalld's first WRITES to `recall.sqlite`.
//!
//! The read routes could only ever be wrong about an answer. These change the
//! database the Python tier also holds, so what is pinned here is the behaviour
//! that makes a write safe to repeat and impossible to corrupt by accident.

use recalld::work::{self, TermError};
use rusqlite::Connection;

fn schema(conn: &Connection) {
    // Copied from `recall.store_schema`, not imported — the Python owns the
    // schema, and a test that re-derived it would test its own copy. The UNIQUE
    // on `term` is what makes the add idempotent, so it must be here.
    conn.execute_batch(
        "CREATE TABLE vocabulary (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            term TEXT NOT NULL UNIQUE,
            created_utc TEXT NOT NULL);
         CREATE TABLE sources (id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL);
         CREATE TABLE refine_requests (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source_id TEXT NOT NULL REFERENCES sources(id),
            start_utc TEXT NOT NULL, end_utc TEXT NOT NULL,
            created_utc TEXT NOT NULL, done_utc TEXT);",
    )
    .expect("schema");
}

fn db() -> Connection {
    let conn = Connection::open_in_memory().expect("open");
    schema(&conn);
    conn
}

const NOW: &str = "2026-09-07T09:00:00+00:00";

#[test]
fn adding_the_same_term_twice_returns_the_same_id_rather_than_failing() {
    // ⚠ The Labels page cannot know what is already in the list before it posts,
    // so a repeat add is ORDINARY, not an error. If this ever throws or inserts a
    // duplicate, the vocabulary grows copies of a household name and the ASR
    // prompt repeats it.
    let conn = db();

    let first = work::add_term(&conn, "vorasidenib", NOW).expect("first");
    let again = work::add_term(&conn, "vorasidenib", NOW).expect("again");

    assert_eq!(first, again);
    assert_eq!(work::vocabulary(&conn).expect("list").items.len(), 1);
}

#[test]
fn a_term_is_trimmed_before_it_is_stored_and_matched() {
    // " EGA wing " and "EGA wing" are the same term to a person. Storing the
    // padded form would defeat the UNIQUE constraint and put two of them in the
    // prompt.
    let conn = db();

    let padded = work::add_term(&conn, "  EGA wing  ", NOW).expect("padded");
    let plain = work::add_term(&conn, "EGA wing", NOW).expect("plain");

    assert_eq!(padded, plain);
    let items = work::vocabulary(&conn).expect("list").items;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].term, "EGA wing");
}

#[test]
fn a_blank_term_is_refused_rather_than_stored() {
    // A blank term would be applied to every transcription as an empty prompt
    // fragment and could never be found again to delete.
    let conn = db();

    assert!(matches!(
        work::add_term(&conn, "   ", NOW),
        Err(TermError::Blank)
    ));
    assert!(matches!(
        work::add_term(&conn, "", NOW),
        Err(TermError::Blank)
    ));
    assert!(work::vocabulary(&conn).expect("list").items.is_empty());
}

#[test]
fn a_database_failure_is_not_reported_as_a_blank_term() {
    // ⚠ These two used to be the SAME value, and the route turned that value
    // into a 400 saying the term was blank. A user shown that message retypes a
    // term that was never the problem, while an unwritable `recall.sqlite` goes
    // uninvestigated because nobody investigates a 400. The distinction has to
    // survive in the type, not in a log line.
    let conn = Connection::open_in_memory().expect("open");
    // No schema: every write fails at the table that is not there.

    let err = work::add_term(&conn, "vorasidenib", NOW).expect_err("no vocabulary table");

    assert!(matches!(err, TermError::Db(_)), "got {err:?}");
}

#[test]
fn terms_list_case_insensitively_so_the_page_reads_alphabetically() {
    // COLLATE NOCASE, not a plain sort: otherwise every capitalised proper noun
    // sorts above every lowercase one, which is most of what this list holds.
    let conn = db();
    for t in ["zebra", "Apple", "mango"] {
        work::add_term(&conn, t, NOW).expect("add");
    }

    let terms: Vec<_> = work::vocabulary(&conn)
        .expect("list")
        .items
        .into_iter()
        .map(|t| t.term)
        .collect();

    assert_eq!(terms, vec!["Apple", "mango", "zebra"]);
}

#[test]
fn deleting_a_term_removes_it_and_deleting_a_missing_one_is_quiet() {
    let conn = db();
    let id = work::add_term(&conn, "vorasidenib", NOW).expect("add");

    work::delete_term(&conn, id).expect("delete");
    assert!(work::vocabulary(&conn).expect("list").items.is_empty());

    // Idempotent: a second delete (a double-tap, a stale page) is not an error.
    work::delete_term(&conn, id).expect("delete again");
}

#[test]
fn a_refine_request_records_the_span_it_was_asked_for() {
    let conn = db();
    conn.execute(
        "INSERT INTO sources (id, name, kind) VALUES ('usb', 'usb', 'coreaudio')",
        (),
    )
    .expect("source");

    work::add_refine_request(
        &conn,
        "usb",
        "2026-09-03T09:00:00+00:00",
        "2026-09-03T10:00:00+00:00",
        NOW,
    )
    .expect("enqueue");

    let (source, start, end): (String, String, String) = conn
        .query_row(
            "SELECT source_id, start_utc, end_utc FROM refine_requests",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("row");
    assert_eq!(source, "usb");
    assert_eq!(start, "2026-09-03T09:00:00+00:00");
    assert_eq!(end, "2026-09-03T10:00:00+00:00");
}

#[test]
fn two_refine_requests_for_one_stretch_both_stand() {
    // ⚠ Deliberately NOT deduplicated. You press "refine this section" again
    // because the first result was wrong; collapsing the second into the first
    // would make the button do nothing precisely when it is needed.
    let conn = db();
    conn.execute(
        "INSERT INTO sources (id, name, kind) VALUES ('usb', 'usb', 'coreaudio')",
        (),
    )
    .expect("source");

    for _ in 0..2 {
        work::add_refine_request(
            &conn,
            "usb",
            "2026-09-03T09:00:00+00:00",
            "2026-09-03T10:00:00+00:00",
            NOW,
        )
        .expect("enqueue");
    }

    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM refine_requests", [], |r| r.get(0))
        .expect("count");
    assert_eq!(n, 2);
}

#[test]
fn the_write_connection_is_separate_from_the_read_only_one() {
    // ⚠ The ownership rule, pinned: a read route must keep taking the read-only
    // handle so a bug in a read path cannot write. If `reads::open` ever starts
    // returning a writable connection this fails, which is the point.
    let dir = tempfile::tempdir().expect("tmp");
    {
        let seed = Connection::open(dir.path().join("recall.sqlite")).expect("create");
        schema(&seed);
    }

    let ro = recalld::reads::open(dir.path()).expect("read-only");
    let err = ro.execute(
        "INSERT INTO vocabulary (term, created_utc) VALUES ('x', '2026-01-01T00:00:00+00:00')",
        (),
    );
    assert!(err.is_err(), "the read handle must not be able to write");

    let rw = work::open_write(dir.path()).expect("writable");
    work::add_term(&rw, "vorasidenib", NOW).expect("the write handle must write");
}
