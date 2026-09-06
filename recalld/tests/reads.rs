//! Stage F1's read routes, against a database built to hold the cases that
//! actually bite.
//!
//! The differential test against the live Python is `scripts/reads_parity.py` —
//! it needs the real archive, so it cannot run here. These are the invariants
//! that must hold everywhere, including on a fresh clone.

use recalld::reads;
use rusqlite::Connection;

/// The subset of `recall.sqlite` these routes read. Copied from
/// `recall.store_schema`, not imported, for the same reason `audiod::store`
/// copies its SQL: the Python owns the schema, and a test that re-derived it
/// would be testing its own copy rather than the shape on disk.
fn schema(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE sources (id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL);
         CREATE TABLE audio_segments (
            id INTEGER PRIMARY KEY, source_id TEXT NOT NULL, path TEXT NOT NULL,
            start_utc TEXT NOT NULL, end_utc TEXT NOT NULL,
            sample_rate INTEGER NOT NULL, channels INTEGER NOT NULL);
         CREATE TABLE transcript_segments (
            id INTEGER PRIMARY KEY, audio_segment_id INTEGER,
            start_utc TEXT NOT NULL, end_utc TEXT NOT NULL, text TEXT NOT NULL,
            language TEXT, asr_confidence REAL, asr_model TEXT, loudness REAL,
            speaker_label TEXT, speaker_id INTEGER, speaker_guess TEXT,
            speaker_score REAL, speaker_cluster TEXT, superseded_by INTEGER,
            provenance TEXT, hidden_reason TEXT, word_timings TEXT);
         CREATE VIRTUAL TABLE transcript_fts USING fts5(text, content='');",
    )
    .expect("schema");
}

#[allow(clippy::too_many_arguments)]
fn turn(conn: &Connection, id: i64, start: &str, text: &str, extra: &[(&str, &str)]) {
    conn.execute(
        "INSERT INTO transcript_segments (id, start_utc, end_utc, text) VALUES (?1, ?2, ?2, ?3)",
        (id, start, text),
    )
    .expect("turn");
    for (col, val) in extra {
        conn.execute(
            &format!("UPDATE transcript_segments SET {col} = ?1 WHERE id = ?2"),
            (*val, id),
        )
        .expect("extra");
    }
    conn.execute(
        "INSERT INTO transcript_fts (rowid, text) VALUES (?1, ?2)",
        (id, text),
    )
    .expect("fts");
}

fn db() -> Connection {
    let conn = Connection::open_in_memory().expect("db");
    schema(&conn);
    conn
}

#[test]
fn a_superseded_or_hidden_turn_is_never_shown() {
    // The single most important property of the read plane: supersession and
    // soft-hiding are how this system corrects itself WITHOUT deleting, so a
    // reader that ignored them would resurrect every wrong transcript ever
    // written and every swept hallucination.
    let conn = db();
    turn(&conn, 1, "2026-09-01T10:00:00+00:00", "current", &[]);
    turn(
        &conn,
        2,
        "2026-09-01T10:00:01+00:00",
        "old",
        &[("superseded_by", "1")],
    );
    turn(
        &conn,
        3,
        "2026-09-01T10:00:02+00:00",
        "junk",
        &[("hidden_reason", "hallucination")],
    );

    let page = reads::timeline(&conn, 50, None).expect("timeline");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].text, "current");

    let hits = reads::search(&conn, "old OR junk OR current", 50).expect("search");
    assert_eq!(hits.items.len(), 1, "search must apply the same projection");
    assert_eq!(hits.items[0].text, "current");
}

#[test]
fn a_human_label_wins_and_drops_the_score_a_guess_keeps_it() {
    // The UI renders "Alice 31%" for a guess and a bare name for a confirmation.
    // Collapsing the two would either hide useful weak guesses or present a
    // machine guess as though a person had confirmed it.
    let conn = db();
    turn(
        &conn,
        1,
        "2026-09-01T10:00:00+00:00",
        "confirmed",
        &[("speaker_label", "Alex"), ("speaker_score", "0.9")],
    );
    turn(
        &conn,
        2,
        "2026-09-01T10:00:01+00:00",
        "guessed",
        &[("speaker_guess", "Sam"), ("speaker_score", "0.31")],
    );

    let page = reads::timeline(&conn, 50, None).expect("timeline");
    let confirmed = &page.items[0];
    let guessed = &page.items[1];

    assert_eq!(confirmed.speaker.as_deref(), Some("Alex"));
    assert!(confirmed.speaker_confirmed);
    assert_eq!(
        confirmed.speaker_confidence, None,
        "a confirmed speaker carries no score — there is nothing to be unsure about"
    );

    assert_eq!(guessed.speaker.as_deref(), Some("Sam"));
    assert!(!guessed.speaker_confirmed);
    assert_eq!(guessed.speaker_confidence, Some(0.31));
}

#[test]
fn the_tier_badge_reports_how_much_processing_a_turn_has_had() {
    let conn = db();
    turn(
        &conn,
        1,
        "2026-09-01T10:00:00+00:00",
        "a",
        &[("asr_model", "human")],
    );
    turn(
        &conn,
        2,
        "2026-09-01T10:00:01+00:00",
        "b",
        &[("asr_model", "live")],
    );
    turn(
        &conn,
        3,
        "2026-09-01T10:00:02+00:00",
        "c",
        &[("provenance", "diarized (mlx-whisper)")],
    );
    turn(
        &conn,
        4,
        "2026-09-01T10:00:03+00:00",
        "d",
        &[("asr_model", "mlx-whisper")],
    );

    let tiers: Vec<&str> = reads::timeline(&conn, 50, None)
        .expect("timeline")
        .items
        .iter()
        .map(|i| i.tier)
        .collect();
    assert_eq!(tiers, ["corrected", "live", "diarized", "transcribed"]);
}

#[test]
fn a_page_boundary_never_splits_turns_that_share_an_instant() {
    // Co-located microphones record the SAME speech, so several turns genuinely
    // carry one start time. A page that cut such a group in half would make the
    // next strict-`<` page skip the remainder — audio silently missing from the
    // timeline, which is the failure this whole system exists to avoid.
    let conn = db();
    turn(&conn, 1, "2026-09-01T10:00:00+00:00", "older", &[]);
    for id in 2..=4 {
        turn(&conn, id, "2026-09-01T10:00:05+00:00", "tied", &[]);
    }

    // limit 2 lands the boundary inside the three-way tie.
    let page = reads::timeline(&conn, 2, None).expect("timeline");
    let tied = page.items.iter().filter(|i| i.text == "tied").count();
    assert_eq!(tied, 3, "the tie group must not be split across pages");
    assert!(page.has_more, "an extended page still has more behind it");

    // Paging on from the tie's instant reaches the older turn exactly once.
    let next = reads::timeline(&conn, 2, Some("2026-09-01T10:00:05+00:00")).expect("next");
    assert_eq!(next.items.len(), 1);
    assert_eq!(next.items[0].text, "older");
}

#[test]
fn a_page_reads_oldest_first_though_the_query_is_newest_first() {
    let conn = db();
    for (id, minute) in [(1, "00"), (2, "01"), (3, "02")] {
        turn(
            &conn,
            id,
            &format!("2026-09-01T10:{minute}:00+00:00"),
            "t",
            &[],
        );
    }
    let page = reads::timeline(&conn, 50, None).expect("timeline");
    let ids: Vec<i64> = page.items.iter().map(|i| i.id).collect();
    assert_eq!(ids, [1, 2, 3], "the page reads top-to-bottom in time order");
    assert!(!page.has_more);
}

#[test]
fn a_turn_with_no_audio_segment_still_appears() {
    // Corrections can exist with no audio row. An INNER join would drop exactly
    // the turns a person took the trouble to fix.
    let conn = db();
    turn(&conn, 1, "2026-09-01T10:00:00+00:00", "corrected", &[]);
    let page = reads::timeline(&conn, 50, None).expect("timeline");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].source, None);
}

#[test]
fn an_empty_page_is_not_treated_as_a_full_one() {
    // Guards the limit-0 edge: Python skips its tie pass on an empty page, and a
    // bare `len == limit` here would run one with no boundary.
    let conn = db();
    let page = reads::timeline(&conn, 0, None).expect("timeline");
    assert!(page.items.is_empty());
}
