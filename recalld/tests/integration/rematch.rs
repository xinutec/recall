//! Re-deriving stale speaker guesses (#1657).
//!
//! ⚠ The behaviour that matters is not "it writes a name" — it is WHICH turns it
//! reconsiders and which it leaves alone. A pass that swept everything every time
//! would rewrite the archive continuously; one that stamped only its rewrites
//! would never finish.

use recalld::rematch::{Pass, run_once};
use rusqlite::Connection;

/// Two voices far apart in the space, so a match is unambiguous.
fn vector(person: u8) -> Vec<f64> {
    let mut v = vec![0.0; 8];
    v[person as usize] = 1.0;
    v
}

fn json(v: &[f64]) -> String {
    serde_json::to_string(v).expect("json")
}

/// The meaning plane, built by the real ladder — not a hand-copied subset.
fn plane() -> Connection {
    let conn = Connection::open_in_memory().expect("db");
    recalld::meaning_schema::ensure(&conn).expect("migrate");
    conn
}

fn enrol(conn: &Connection, person: &str, v: &[f64], created: &str) {
    conn.execute(
        "INSERT OR IGNORE INTO speakers (name) VALUES (?1)",
        [person],
    )
    .expect("speaker");
    let id: i64 = conn
        .query_row("SELECT id FROM speakers WHERE name = ?1", [person], |r| {
            r.get(0)
        })
        .expect("speaker id");
    conn.execute(
        "INSERT INTO speaker_embeddings (speaker_id, vector, created_utc) VALUES (?1, ?2, ?3)",
        rusqlite::params![id, json(v), created],
    )
    .expect("print");
}

/// A turn with an embedding and whatever guess it was born with.
fn turn(conn: &Connection, id: i64, v: &[f64], guess: Option<(&str, f64)>) {
    conn.execute(
        "INSERT OR IGNORE INTO sources (id, name, kind) VALUES ('usb', 'usb', 'MIC')",
        [],
    )
    .expect("source");
    conn.execute(
        "INSERT INTO audio_segments (id, source_id, path, start_utc, end_utc, sample_rate, channels)
         VALUES (?1, 'usb', '/x', ?2, '2026-09-01T10:01:00+00:00', 16000, 1)",
        rusqlite::params![id, format!("2026-09-01T10:{id:02}:00+00:00")],
    )
    .expect("audio segment");
    conn.execute(
        "INSERT INTO transcript_segments
             (id, audio_segment_id, start_utc, end_utc, text, asr_model, speaker_guess, speaker_score)
         VALUES (?1, ?1, '2026-09-01T10:00:00+00:00', '2026-09-01T10:00:05+00:00', 'hello', 'm', ?2, ?3)",
        rusqlite::params![id, guess.map(|g| g.0), guess.map(|g| g.1)],
    )
    .expect("turn");
    conn.execute(
        "INSERT INTO transcript_embeddings (segment_id, vector) VALUES (?1, ?2)",
        rusqlite::params![id, json(v)],
    )
    .expect("embedding");
}

fn guess_of(conn: &Connection, id: i64) -> (Option<String>, Option<String>) {
    conn.query_row(
        "SELECT speaker_guess, speaker_matched_utc FROM transcript_segments WHERE id = ?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .expect("row")
}

#[test]
fn a_guess_made_before_the_newest_enrolment_is_re_derived() {
    // ⚠ THE WHOLE POINT. The turn was named Alex when Alex was the only enrolled
    // voice; Sam was enrolled later and is the better match. Nothing revisited it
    // until this pass existed — 0.491 stored against 0.913 re-derived, measured
    // on the fleet.
    let mut conn = plane();
    enrol(&conn, "Alex", &vector(0), "2026-08-01T00:00:00Z");
    turn(&conn, 1, &vector(3), Some(("Alex", 0.9)));
    enrol(&conn, "Sam", &vector(3), "2026-09-01T00:00:00Z");

    let pass = run_once(&mut conn, 10, "2026-09-18T12:00:00Z").expect("pass");
    assert_eq!(pass.examined, 1);
    assert_eq!(pass.rewritten, 1);
    let (name, stamped) = guess_of(&conn, 1);
    assert_eq!(name.as_deref(), Some("Sam"));
    assert!(stamped.is_some(), "an examined turn must be stamped");
}

#[test]
fn a_turn_whose_answer_is_unchanged_is_stamped_not_rewritten() {
    // ⚠ **Stamping the UNCHANGED ones is what makes the pass finish.** Marking
    // only rewrites would bring every settled turn back on every run, for ever.
    //
    // ⚠ Reaching that branch takes three runs, and an earlier version of this
    // test did not: a turn with no guess is REWRITTEN on its first pass, so the
    // stamp under test was the rewrite branch's. Deleting the unchanged branch's
    // stamp left the suite green. The sequence below settles the turn (rewrite),
    // re-arms it with a new voice (unchanged — the answer still holds), and only
    // then asks whether it was stamped.
    let mut conn = plane();
    enrol(&conn, "Alex", &vector(0), "2026-08-01T00:00:00Z");
    turn(&conn, 1, &vector(0), None);

    let settle = run_once(&mut conn, 10, "2026-09-18T12:00:00Z").expect("settle");
    assert_eq!(settle.rewritten, 1, "a turn with no guess gains one");

    enrol(&conn, "Sam", &vector(5), "2026-09-18T13:00:00Z");
    let reconsidered = run_once(&mut conn, 10, "2026-09-18T14:00:00Z").expect("reconsider");
    assert_eq!(
        reconsidered.unchanged, 1,
        "the new voice does not change this answer — the branch under test"
    );

    let after = run_once(&mut conn, 10, "2026-09-18T15:00:00Z").expect("after");
    assert_eq!(
        after,
        Pass::default(),
        "an unchanged turn must be settled too, or the pass never finishes"
    );
}

#[test]
fn enrolling_a_voice_re_arms_the_whole_archive() {
    // The only event that can change an answer, and the pass must notice it
    // without anyone scheduling a sweep.
    let mut conn = plane();
    enrol(&conn, "Alex", &vector(0), "2026-08-01T00:00:00Z");
    turn(&conn, 1, &vector(0), None);
    run_once(&mut conn, 10, "2026-09-18T12:00:00Z").expect("settle");
    assert_eq!(
        run_once(&mut conn, 10, "2026-09-18T12:30:00Z").expect("quiet"),
        Pass::default()
    );

    enrol(&conn, "Sam", &vector(5), "2026-09-18T14:00:00Z");
    let after = run_once(&mut conn, 10, "2026-09-18T15:00:00Z").expect("re-armed");
    assert_eq!(after.examined, 1, "a new voiceprint reopens every turn");
    assert_eq!(after.unchanged, 1, "and this one still answers the same");
}

#[test]
fn a_turn_nothing_matches_keeps_the_name_it_had() {
    // ⚠ "No match today" is not evidence the old name was wrong. Blanking it
    // would trade an answer for nothing.
    let mut conn = plane();
    enrol(&conn, "Alex", &vector(0), "2026-08-01T00:00:00Z");
    turn(&conn, 1, &vector(0), Some(("Alex", 0.9)));
    // Drop every print, then enrol someone new so the pass has a reason to run.
    conn.execute("DELETE FROM speaker_embeddings", [])
        .expect("clear");
    conn.execute(
        "INSERT INTO speaker_embeddings (speaker_id, vector, created_utc)
         SELECT id, '[]', '2026-09-18T14:00:00Z' FROM speakers WHERE name = 'Alex'",
        [],
    )
    .expect("an unparseable print");

    let pass = run_once(&mut conn, 10, "2026-09-18T15:00:00Z").expect("pass");
    assert_eq!(pass.unmatched, 1);
    assert_eq!(guess_of(&conn, 1).0.as_deref(), Some("Alex"));
}

#[test]
fn with_nobody_enrolled_it_does_nothing_rather_than_stamping() {
    // ⚠ Stamping here would mark every turn fresh against an EMPTY corpus, so the
    // first real enrolment would look like it had already been applied.
    let mut conn = plane();
    turn(&conn, 1, &vector(0), Some(("Alex", 0.9)));
    assert_eq!(
        run_once(&mut conn, 10, "2026-09-18T12:00:00Z").expect("pass"),
        Pass::default()
    );
    assert!(guess_of(&conn, 1).1.is_none(), "nothing may be stamped");
}

#[test]
fn a_human_label_is_never_touched() {
    // The one line that must not move: `speaker_label` is what a person typed.
    let mut conn = plane();
    enrol(&conn, "Alex", &vector(0), "2026-08-01T00:00:00Z");
    turn(&conn, 1, &vector(3), Some(("Alex", 0.9)));
    conn.execute(
        "UPDATE transcript_segments SET speaker_label = 'Karthica' WHERE id = 1",
        [],
    )
    .expect("label");
    enrol(&conn, "Sam", &vector(3), "2026-09-01T00:00:00Z");

    run_once(&mut conn, 10, "2026-09-18T12:00:00Z").expect("pass");
    let label: String = conn
        .query_row(
            "SELECT speaker_label FROM transcript_segments WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .expect("label");
    assert_eq!(label, "Karthica");
    assert_eq!(guess_of(&conn, 1).0.as_deref(), Some("Sam"));
}

#[test]
fn the_batch_is_bounded_and_oldest_guesses_are_not_starved() {
    let mut conn = plane();
    enrol(&conn, "Alex", &vector(0), "2026-08-01T00:00:00Z");
    for id in 1..=5 {
        turn(&conn, id, &vector(0), None);
    }
    enrol(&conn, "Sam", &vector(5), "2026-09-01T00:00:00Z");

    let first = run_once(&mut conn, 2, "2026-09-18T12:00:00Z").expect("first");
    assert_eq!(first.examined, 2, "the limit is respected");
    let second = run_once(&mut conn, 2, "2026-09-18T12:01:00Z").expect("second");
    assert_eq!(second.examined, 2, "and the next batch is different turns");
    let third = run_once(&mut conn, 10, "2026-09-18T12:02:00Z").expect("third");
    assert_eq!(
        third.examined, 1,
        "five turns, drained in three bounded passes"
    );
}
