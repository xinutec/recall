//! Vocabulary writes to `recall.sqlite`: what makes a write safe to repeat and
//! hard to corrupt by accident.

use recalld::work::{self, TermError};
use rusqlite::Connection;

fn schema(conn: &Connection) {
    recalld::meaning_schema::ensure(conn).expect("schema");
}

fn db() -> Connection {
    let conn = Connection::open_in_memory().expect("open");
    schema(&conn);
    conn
}

const NOW: &str = "2026-09-07T09:00:00+00:00";

#[test]
fn adding_the_same_term_twice_returns_the_same_id_rather_than_failing() {
    // The Labels page cannot know what is already listed before it posts, so a
    // repeat add is ordinary. A duplicate row would repeat the term in the ASR
    // prompt.
    let conn = db();

    let first = work::add_term(&conn, "vorasidenib", &crate::stamp(NOW)).expect("first");
    let again = work::add_term(&conn, "vorasidenib", &crate::stamp(NOW)).expect("again");

    assert_eq!(first, again);
    assert_eq!(work::vocabulary(&conn).expect("list").items.len(), 1);
}

#[test]
fn a_term_is_trimmed_before_it_is_stored_and_matched() {
    // Storing the padded form would defeat the UNIQUE constraint and put two
    // copies in the prompt.
    let conn = db();

    let padded = work::add_term(&conn, "  EGA wing  ", &crate::stamp(NOW)).expect("padded");
    let plain = work::add_term(&conn, "EGA wing", &crate::stamp(NOW)).expect("plain");

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
        work::add_term(&conn, "   ", &crate::stamp(NOW)),
        Err(TermError::Blank)
    ));
    assert!(matches!(
        work::add_term(&conn, "", &crate::stamp(NOW)),
        Err(TermError::Blank)
    ));
    assert!(work::vocabulary(&conn).expect("list").items.is_empty());
}

#[test]
fn a_database_failure_is_not_reported_as_a_blank_term() {
    // A database failure reported as a blank-term 400 sends the user retyping a
    // term that was never the problem, while nobody investigates the unwritable
    // database.
    let conn = Connection::open_in_memory().expect("open");
    // No schema: every write fails at the table that is not there.

    let err =
        work::add_term(&conn, "vorasidenib", &crate::stamp(NOW)).expect_err("no vocabulary table");

    assert!(matches!(err, TermError::Db(_)), "got {err:?}");
}

#[test]
fn terms_list_case_insensitively_so_the_page_reads_alphabetically() {
    // COLLATE NOCASE, not a plain sort: otherwise every capitalised proper noun
    // sorts above every lowercase one, which is most of what this list holds.
    let conn = db();
    for t in ["zebra", "Apple", "mango"] {
        work::add_term(&conn, t, &crate::stamp(NOW)).expect("add");
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
    let id = work::add_term(&conn, "vorasidenib", &crate::stamp(NOW)).expect("add");

    work::delete_term(&conn, id).expect("delete");
    assert!(work::vocabulary(&conn).expect("list").items.is_empty());

    // Idempotent: a second delete (a double-tap, a stale page) is not an error.
    work::delete_term(&conn, id).expect("delete again");
}

#[test]
fn the_write_connection_is_separate_from_the_read_only_one() {
    // A read route takes the read-only handle, so a bug in a read path cannot
    // write.
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
    work::add_term(&rw, "vorasidenib", &crate::stamp(NOW)).expect("the write handle must write");
}
