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

/// A voice on A's recording is guessed among A's people only: B's person with
/// the very same print is never offered.
#[test]
fn a_guess_is_made_only_among_the_recordings_owners_people() {
    let f = crate::fixture::Fleet::new();
    let clip = f.clip(
        "phone-a",
        "tcp_pcm",
        "20260101T080000",
        &[(1.0, 5.0, "hello there")],
        Some(&[("SPEAKER_00", 0.0, 60.0)]),
    );
    f.ingest
        .execute(
            "UPDATE jobs SET result = ?1 WHERE filename = ?2 AND kind = ?3",
            (
                r#"{"ok":true,"result":{"turns":[{"speaker":"SPEAKER_00","start":0.0,"end":60.0}],"speakers":[{"speaker":"SPEAKER_00","vector":[1.0,0.0]}]}}"#,
                &clip.filename,
                audiocore::job::Kind::DiarizeSegment,
            ),
        )
        .expect("a voice with a print");
    f.meaning
        .execute_batch(
            "UPDATE sources SET owner = 'a' WHERE id = 'phone-a';
             INSERT INTO speakers (id, owner, name) VALUES (1, 'a', 'Alma'), (2, 'b', 'Bertha');
             INSERT INTO speaker_embeddings (speaker_id, vector, created_utc) VALUES
                 (1, '[0.0,1.0]', '2026-01-01T00:00:00+00:00'),
                 (2, '[1.0,0.0]', '2026-01-01T00:00:00+00:00');",
        )
        .expect("two people's prints");

    f.refresh();

    let guess: Option<String> = f
        .meaning
        .query_row(
            "SELECT speaker_guess FROM rendered_lines WHERE text LIKE '%hello%' AND hidden_reason IS NULL",
            [],
            |r| r.get(0),
        )
        .expect("the line");
    assert_eq!(guess.as_deref(), Some("Alma"));
}

/// With more than one person able to sign in, something nobody owns would be
/// nobody's to see or everyone's: the server refuses to start until it is
/// claimed. One person alone may start with it unclaimed.
#[test]
fn more_than_one_person_needs_everything_owned() {
    let conn = db();
    let one: std::collections::HashSet<String> = ["a".to_owned()].into();
    let two: std::collections::HashSet<String> = ["a".to_owned(), "b".to_owned()].into();
    let anyone = std::collections::HashSet::new();
    assert!(recalld::owners::ready_for(&conn, &one).is_ok());
    assert!(recalld::owners::ready_for(&conn, &two).is_err());
    assert!(
        recalld::owners::ready_for(&conn, &anyone).is_err(),
        "an empty list admits anyone"
    );
    claim_unowned(&conn, "a", true).expect("claim");
    assert!(recalld::owners::ready_for(&conn, &two).is_ok());
}
