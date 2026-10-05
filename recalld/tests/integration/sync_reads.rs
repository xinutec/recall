//! The sync plane's reads, and the live feed's writes.

use recalld::labels::initial_prompt;
use rusqlite::Connection;

fn store() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    recalld::meaning_schema::ensure(&conn).expect("schema");
    conn
}

fn glossary(conn: &Connection, speakers: &[&str], terms: &[&str]) {
    for name in speakers {
        conn.execute("INSERT INTO speakers (name) VALUES (?1)", [name])
            .unwrap();
    }
    for term in terms {
        conn.execute(
            "INSERT INTO vocabulary (term, created_utc) VALUES (?1, '2026-09-09T00:00:00+00:00')",
            [term],
        )
        .unwrap();
    }
}

/// The expected string came from the Python it replaced. Speaker names come
/// before the vocabulary, a term in both appears once, and each group is
/// ordered `COLLATE NOCASE`.
#[test]
fn the_glossary_prompt_matches_what_the_python_built() {
    let conn = store();
    glossary(
        &conn,
        &["Oskar", "Lotte", "Zebra"],
        &[
            "apple",
            "Lotte",
            "banana",
            &"x".repeat(580),
            "never-reached",
        ],
    );

    assert_eq!(
        initial_prompt(&conn).unwrap().as_deref(),
        Some("Lotte, Oskar, Zebra, apple, banana, never-reached")
    );
}

/// The cap ends the list rather than skipping a long term, so priority decides,
/// not length.
#[test]
fn a_term_over_the_cap_ends_the_list_rather_than_being_skipped() {
    let conn = store();
    // The middle term overflows the cap on its own (3 + 2 + 700 = 705), so the
    // behaviours differ: break gives "aaa", skip would give "aaa, ccc".
    glossary(&conn, &[], &["aaa", &"b".repeat(700), "ccc"]);

    let prompt = initial_prompt(&conn).unwrap().expect("a prompt");

    assert_eq!(
        prompt, "aaa",
        "the list must END at the first term over the cap, not skip it and carry on"
    );
}

#[test]
fn an_empty_glossary_is_none_not_an_empty_string() {
    // Null, not "": the Mac branches on null.
    assert_eq!(initial_prompt(&store()).unwrap(), None);
}

// --- the live feed -----------------------------------------------------------

use recalld::work::{LiveTurn, ingest_live};

/// The real migration ladder.
fn live_store() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    recalld::meaning_schema::ensure(&conn).expect("schema");
    conn
}

/// Any delivery instant; only the test that asserts on it cares which.
fn delivered() -> chrono::DateTime<chrono::Utc> {
    "2026-09-09T10:00:31.500000+00:00".parse().unwrap()
}

fn a_turn(start: &str, text: &str) -> LiveTurn {
    LiveTurn {
        start: start.to_owned(),
        end: "2026-09-09T10:00:05+00:00".to_owned(),
        text: text.to_owned(),
        asr_model: "live".to_owned(),
        language: Some("en".to_owned()),
    }
}

/// `start_utc` is when the words were said; `created_utc` when they were
/// delivered, the tier's only record of its latency.
#[test]
fn a_live_turn_records_when_it_was_delivered_not_only_when_it_was_said() {
    let mut conn = live_store();

    ingest_live(
        &mut conn,
        &[a_turn("2026-09-09T10:00:00+00:00", "hello there")],
        delivered(),
    )
    .unwrap();

    let (said, stored): (String, String) = conn
        .query_row(
            "SELECT start_utc, created_utc FROM transcript_segments",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("the turn carries both instants");
    assert_eq!(said, "2026-09-09T10:00:00+00:00");
    assert_eq!(
        stored, "2026-09-09T10:00:31.500000+00:00",
        "the delivery instant is what makes live latency measurable after the fact"
    );
}

/// The writer fills `transcript_fts`; a missing row is silently unsearchable.
#[test]
fn a_stored_live_turn_is_searchable() {
    let mut conn = live_store();

    let stored = ingest_live(
        &mut conn,
        &[a_turn("2026-09-09T10:00:00+00:00", "hello there")],
        delivered(),
    )
    .unwrap();

    assert_eq!(stored, 1);
    let found: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM transcript_fts WHERE transcript_fts MATCH 'hello'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(found, 1, "the turn exists but cannot be searched for");
}

/// A re-push neither duplicates a turn nor brings back a reconciled one.
#[test]
fn a_repushed_turn_is_skipped_even_once_hidden() {
    let mut conn = live_store();
    let turns = [a_turn("2026-09-09T10:00:00+00:00", "same words")];

    assert_eq!(ingest_live(&mut conn, &turns, delivered()).unwrap(), 1);
    assert_eq!(
        ingest_live(&mut conn, &turns, delivered()).unwrap(),
        0,
        "a retry duplicated it"
    );

    // The archive reconciles it away; a later retry must still not bring it back.
    conn.execute(
        "UPDATE transcript_segments SET hidden_reason = 'reconciled'",
        [],
    )
    .unwrap();

    assert_eq!(
        ingest_live(&mut conn, &turns, delivered()).unwrap(),
        0,
        "a hidden turn was resurrected"
    );
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM transcript_segments", [], |r| r.get(0))
        .unwrap();
    assert_eq!(total, 1);
}

/// The presence check compares the stored spelling, so a retry matches.
#[test]
fn a_z_suffixed_time_matches_the_offset_spelling_it_was_stored_as() {
    let mut conn = live_store();

    assert_eq!(
        ingest_live(
            &mut conn,
            &[a_turn("2026-09-09T10:00:00+00:00", "x")],
            delivered()
        )
        .unwrap(),
        1
    );
    // The same instant, spelled the other way round.
    assert_eq!(
        ingest_live(
            &mut conn,
            &[a_turn("2026-09-09T10:00:00Z", "x")],
            delivered()
        )
        .unwrap(),
        0
    );

    let stored: String = conn
        .query_row("SELECT start_utc FROM transcript_segments", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        stored, "2026-09-09T10:00:00+00:00",
        "Z must be stored as +00:00"
    );
}

#[test]
fn an_unparseable_time_costs_that_turn_and_no_other() {
    let mut conn = live_store();

    let stored = ingest_live(
        &mut conn,
        &[
            a_turn("not a time", "dropped"),
            a_turn("2026-09-09T10:00:00+00:00", "kept"),
        ],
        delivered(),
    )
    .unwrap();

    assert_eq!(stored, 1, "one bad turn must not cost the batch");
    let text: String = conn
        .query_row("SELECT text FROM transcript_segments", [], |r| r.get(0))
        .unwrap();
    assert_eq!(text, "kept");
}

/// Live turns are short and hard, what Whisper loops on: loops are dropped
/// here, the same for every pusher.
#[test]
fn a_degenerate_loop_is_not_stored_as_a_live_turn() {
    let mut conn = live_store();

    assert_eq!(
        ingest_live(
            &mut conn,
            &[a_turn(
                "2026-09-09T10:00:00+00:00",
                "goog goog goog goog goog goog"
            )],
            delivered(),
        )
        .unwrap(),
        0
    );
    assert_eq!(
        ingest_live(
            &mut conn,
            &[a_turn("2026-09-09T10:00:01+00:00", "... ***")],
            delivered()
        )
        .unwrap(),
        0
    );
    // And real speech still lands, so the filter is not simply refusing.
    assert_eq!(
        ingest_live(
            &mut conn,
            &[a_turn(
                "2026-09-09T10:00:02+00:00",
                "we should leave at eight"
            )],
            delivered(),
        )
        .unwrap(),
        1
    );
}

/// On audio it cannot place, the model emits a prompt name, mostly in short
/// live turns. A false name passes every other signal.
#[test]
fn a_live_turn_that_is_nothing_but_a_household_name_is_refused() {
    let mut conn = live_store();
    conn.execute("INSERT INTO speakers (name) VALUES ('Anna')", [])
        .unwrap();

    let stored = ingest_live(
        &mut conn,
        &[
            a_turn("2026-09-09T10:00:00+00:00", "Anna."),
            a_turn("2026-09-09T10:00:01+00:00", " anna "),
            a_turn("2026-09-09T10:00:02+00:00", "Anna, are you there?"),
            a_turn("2026-09-09T10:00:03+00:00", "Annabel"),
        ],
        delivered(),
    )
    .unwrap();

    // Only a bare name goes.
    assert_eq!(stored, 2, "a name in a sentence must survive");
    let kept: Vec<String> = conn
        .prepare("SELECT text FROM transcript_segments ORDER BY start_utc")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(kept, vec!["Anna, are you there?", "Annabel"]);
}

/// The refusal is scoped to enrolled names; with no speaker enrolled it does
/// nothing.
#[test]
fn with_nobody_enrolled_the_bare_name_rule_refuses_nothing() {
    let mut conn = live_store();
    let stored = ingest_live(
        &mut conn,
        &[a_turn("2026-09-09T10:00:00+00:00", "Anna.")],
        delivered(),
    )
    .unwrap();
    assert_eq!(stored, 1);
}
