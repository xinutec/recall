//! Re-deriving stale speaker guesses. What matters is which turns a pass
//! reconsiders and which it leaves alone: sweeping everything every time would
//! rewrite the archive continuously, and stamping only rewrites would never
//! finish.

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

/// The meaning plane, built by the real migration ladder, not a hand-copied
/// subset.
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
    // The turn was named Alex when Alex was the only enrolled voice; Sam,
    // enrolled later, is the better match.
    let mut conn = plane();
    enrol(&conn, "Alex", &vector(0), "2026-08-01T00:00:00+00:00");
    turn(&conn, 1, &vector(3), Some(("Alex", 0.9)));
    enrol(&conn, "Sam", &vector(3), "2026-09-01T00:00:00+00:00");

    let pass = run_once(&mut conn, 10, &crate::stamp("2026-09-18T12:00:00+00:00")).expect("pass");
    assert_eq!(pass.examined, 1);
    assert_eq!(pass.rewritten, 1);
    let (name, stamped) = guess_of(&conn, 1);
    assert_eq!(name.as_deref(), Some("Sam"));
    assert!(stamped.is_some(), "an examined turn must be stamped");
}

#[test]
fn a_turn_whose_answer_is_unchanged_is_stamped_not_rewritten() {
    // Stamping unchanged turns is what makes the pass finish; marking only
    // rewrites would bring every settled turn back on every run.
    //
    // ⚠ Reaching that branch takes three runs: a turn with no guess is rewritten
    // on its first pass, so the sequence settles it (rewrite), re-arms it with a
    // new voice (unchanged), and only then checks the stamp.
    let mut conn = plane();
    enrol(&conn, "Alex", &vector(0), "2026-08-01T00:00:00+00:00");
    turn(&conn, 1, &vector(0), None);

    let settle =
        run_once(&mut conn, 10, &crate::stamp("2026-09-18T12:00:00+00:00")).expect("settle");
    assert_eq!(settle.rewritten, 1, "a turn with no guess gains one");

    enrol(&conn, "Sam", &vector(5), "2026-09-18T13:00:00+00:00");
    let reconsidered =
        run_once(&mut conn, 10, &crate::stamp("2026-09-18T14:00:00+00:00")).expect("reconsider");
    assert_eq!(
        reconsidered.unchanged, 1,
        "the new voice does not change this answer — the branch under test"
    );

    let after = run_once(&mut conn, 10, &crate::stamp("2026-09-18T15:00:00+00:00")).expect("after");
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
    enrol(&conn, "Alex", &vector(0), "2026-08-01T00:00:00+00:00");
    turn(&conn, 1, &vector(0), None);
    run_once(&mut conn, 10, &crate::stamp("2026-09-18T12:00:00+00:00")).expect("settle");
    assert_eq!(
        run_once(&mut conn, 10, &crate::stamp("2026-09-18T12:30:00+00:00")).expect("quiet"),
        Pass::default()
    );

    enrol(&conn, "Sam", &vector(5), "2026-09-18T14:00:00+00:00");
    let after =
        run_once(&mut conn, 10, &crate::stamp("2026-09-18T15:00:00+00:00")).expect("re-armed");
    assert_eq!(after.examined, 1, "a new voiceprint reopens every turn");
    assert_eq!(after.unchanged, 1, "and this one still answers the same");
}

#[test]
fn a_turn_nothing_matches_keeps_the_name_it_had() {
    // No match today is not evidence the old name was wrong.
    let mut conn = plane();
    enrol(&conn, "Alex", &vector(0), "2026-08-01T00:00:00+00:00");
    turn(&conn, 1, &vector(0), Some(("Alex", 0.9)));
    // Drop every print, then enrol someone new so the pass has a reason to run.
    conn.execute("DELETE FROM speaker_embeddings", [])
        .expect("clear");
    conn.execute(
        "INSERT INTO speaker_embeddings (speaker_id, vector, created_utc)
         SELECT id, '[]', '2026-09-18T14:00:00+00:00' FROM speakers WHERE name = 'Alex'",
        [],
    )
    .expect("an unparseable print");

    let pass = run_once(&mut conn, 10, &crate::stamp("2026-09-18T15:00:00+00:00")).expect("pass");
    assert_eq!(pass.unmatched, 1);
    assert_eq!(guess_of(&conn, 1).0.as_deref(), Some("Alex"));
}

#[test]
fn with_nobody_enrolled_it_does_nothing_rather_than_stamping() {
    // Stamping against an empty corpus would make the first real enrolment look
    // already applied.
    let mut conn = plane();
    turn(&conn, 1, &vector(0), Some(("Alex", 0.9)));
    assert_eq!(
        run_once(&mut conn, 10, &crate::stamp("2026-09-18T12:00:00+00:00")).expect("pass"),
        Pass::default()
    );
    assert!(guess_of(&conn, 1).1.is_none(), "nothing may be stamped");
}

#[test]
fn a_human_label_is_never_touched() {
    // The one line that must not move: `speaker_label` is what a person typed.
    let mut conn = plane();
    enrol(&conn, "Alex", &vector(0), "2026-08-01T00:00:00+00:00");
    turn(&conn, 1, &vector(3), Some(("Alex", 0.9)));
    conn.execute(
        "UPDATE transcript_segments SET speaker_label = 'Karthica' WHERE id = 1",
        [],
    )
    .expect("label");
    enrol(&conn, "Sam", &vector(3), "2026-09-01T00:00:00+00:00");

    run_once(&mut conn, 10, &crate::stamp("2026-09-18T12:00:00+00:00")).expect("pass");
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
    enrol(&conn, "Alex", &vector(0), "2026-08-01T00:00:00+00:00");
    for id in 1..=5 {
        turn(&conn, id, &vector(0), None);
    }
    enrol(&conn, "Sam", &vector(5), "2026-09-01T00:00:00+00:00");

    let first = run_once(&mut conn, 2, &crate::stamp("2026-09-18T12:00:00+00:00")).expect("first");
    assert_eq!(first.examined, 2, "the limit is respected");
    let second =
        run_once(&mut conn, 2, &crate::stamp("2026-09-18T12:01:00+00:00")).expect("second");
    assert_eq!(second.examined, 2, "and the next batch is different turns");
    let third = run_once(&mut conn, 10, &crate::stamp("2026-09-18T12:02:00+00:00")).expect("third");
    assert_eq!(
        third.examined, 1,
        "five turns, drained in three bounded passes"
    );
}
