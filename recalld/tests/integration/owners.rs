//! Whose recordings and names are whose: the claim that gives everything
//! recorded before owners existed to one person.

use recalld::owners::claim_unowned;

fn db() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().expect("db");
    recalld::meaning_schema::ensure(&conn).expect("schema");
    conn.execute_batch(
        "INSERT INTO sources (id, name, kind) VALUES ('usb', 'USB', 'coreaudio');
         INSERT INTO sources (id, name, kind, owner) VALUES ('phone', 'Phone', 'tcp_pcm', 'b');
         INSERT INTO speakers (name) VALUES ('Alex');
         INSERT INTO speakers (owner, name) VALUES ('b', 'Sam');",
    )
    .expect("rows");
    conn
}

fn owners(conn: &rusqlite::Connection, table: &str) -> Vec<(String, Option<String>)> {
    let key = if table == "sources" { "id" } else { "name" };
    conn.prepare(&format!("SELECT {key}, owner FROM {table} ORDER BY {key}"))
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("rows")
        .collect::<Result<_, _>>()
        .expect("collect")
}

#[test]
fn a_dry_run_counts_what_nobody_owns_and_changes_nothing() {
    let conn = db();
    let seen = claim_unowned(&conn, "a", false).expect("dry run");
    assert_eq!((seen.sources, seen.speakers, seen.applied), (1, 1, false));
    assert_eq!(owners(&conn, "sources")[1], ("usb".to_owned(), None));
}

#[test]
fn applied_it_gives_only_the_unowned_to_the_one_named() {
    let conn = db();
    let done = claim_unowned(&conn, "a", true).expect("claim");
    assert_eq!((done.sources, done.speakers, done.applied), (1, 1, true));
    assert_eq!(
        owners(&conn, "sources"),
        [
            ("phone".to_owned(), Some("b".to_owned())),
            ("usb".to_owned(), Some("a".to_owned()))
        ]
    );
    assert_eq!(
        owners(&conn, "speakers"),
        [
            ("Alex".to_owned(), Some("a".to_owned())),
            ("Sam".to_owned(), Some("b".to_owned()))
        ]
    );
    // Nothing left to claim.
    let again = claim_unowned(&conn, "a", true).expect("again");
    assert_eq!((again.sources, again.speakers), (0, 0));
}
