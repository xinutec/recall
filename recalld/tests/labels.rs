//! The labelling surface's read half (stage F1).
//!
//! Three routes, and each has one rule that a plain SELECT would get wrong. All
//! three failures are quiet — a list renders, a clip plays, autocomplete offers
//! something — so they are pinned rather than left to be noticed.

use recalld::labels::{
    correction_window, corrections_by_speaker, known_speaker_names, list_corrections,
};
use rusqlite::Connection;

fn schema(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE speakers (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
         CREATE TABLE audio_segments (
            id INTEGER PRIMARY KEY, source_id TEXT NOT NULL, path TEXT NOT NULL,
            start_utc TEXT NOT NULL, end_utc TEXT NOT NULL,
            sample_rate INTEGER NOT NULL, channels INTEGER NOT NULL);
         CREATE TABLE transcript_segments (
            id INTEGER PRIMARY KEY, audio_segment_id INTEGER,
            start_utc TEXT NOT NULL, end_utc TEXT NOT NULL, text TEXT NOT NULL,
            speaker_label TEXT, superseded_by INTEGER, hidden_reason TEXT);
         CREATE TABLE corrections (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            transcript_segment_id INTEGER, audio_segment_id INTEGER,
            start_utc TEXT NOT NULL, end_utc TEXT NOT NULL,
            original_text TEXT NOT NULL, corrected_text TEXT NOT NULL,
            language TEXT, created_utc TEXT NOT NULL,
            speaker TEXT, hidden_reason TEXT, audio_confidence REAL);",
    )
    .expect("schema");
}

fn db() -> Connection {
    let conn = Connection::open_in_memory().expect("open");
    schema(&conn);
    conn
}

fn correction(conn: &Connection, id: i64, speaker: Option<&str>, hidden: Option<&str>) {
    conn.execute(
        "INSERT INTO corrections
           (id, start_utc, end_utc, original_text, corrected_text, created_utc, speaker, hidden_reason)
         VALUES (?1, '2026-06-14T18:00:00+00:00', '2026-06-14T18:00:04+00:00',
                 'orig', 'fixed', '2026-06-14T18:00:00+00:00', ?2, ?3)",
        rusqlite::params![id, speaker, hidden],
    )
    .expect("correction");
}

#[test]
fn the_roster_never_offers_a_diarization_cluster_tag_as_a_name() {
    // ⚠ `SPEAKER_00` is not a person. Offering it for autocomplete spreads a
    // machine tag into the household roster one accepted suggestion at a time,
    // and it looks like a real name in every list afterwards.
    let conn = db();
    conn.execute("INSERT INTO speakers (id, name) VALUES (1, 'Pippijn')", ())
        .expect("speaker");
    conn.execute(
        "INSERT INTO transcript_segments (id, start_utc, end_utc, text, speaker_label)
         VALUES (1, '2026-06-14T18:00:00+00:00', '2026-06-14T18:00:01+00:00', 'x', 'SPEAKER_01'),
                (2, '2026-06-14T18:00:01+00:00', '2026-06-14T18:00:02+00:00', 'y', 'Dr Lee')",
        (),
    )
    .expect("turns");

    let names = known_speaker_names(&conn).expect("roster").names;

    assert!(names.contains(&"Pippijn".to_string()));
    assert!(names.contains(&"Dr Lee".to_string()));
    assert!(
        !names.iter().any(|n| n.starts_with("SPEAKER")),
        "got {names:?}"
    );
}

#[test]
fn the_roster_is_case_insensitively_ordered_and_free_of_blanks() {
    let conn = db();
    conn.execute(
        "INSERT INTO speakers (id, name) VALUES (1, 'zoe'), (2, 'Alex'), (3, '')",
        (),
    )
    .expect("speakers");

    let names = known_speaker_names(&conn).expect("roster").names;

    assert_eq!(names, vec!["Alex", "zoe"]);
}

#[test]
fn a_hidden_correction_stays_out_of_the_review_list() {
    // ⚠ The whole point of hiding one is that it was poisoning enrolment — a
    // mistaken label feeding the voiceprints. Listing it again would invite
    // re-confirming the mistake.
    let conn = db();
    correction(&conn, 1, Some("Pippijn"), None);
    correction(&conn, 2, Some("Pippijn"), Some("wrong speaker"));

    let items = list_corrections(&conn, None, 200).expect("list");

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, 1);
}

#[test]
fn corrections_are_newest_first_and_filterable_by_voice() {
    let conn = db();
    correction(&conn, 1, Some("Alex"), None);
    correction(&conn, 2, Some("Pippijn"), None);
    correction(&conn, 3, Some("Alex"), None);

    let all = list_corrections(&conn, None, 200).expect("all");
    assert_eq!(
        all.iter().map(|l| l.id).collect::<Vec<_>>(),
        vec![3, 2, 1],
        "newest first"
    );

    let alex = list_corrections(&conn, Some("Alex"), 200).expect("filtered");
    assert_eq!(alex.iter().map(|l| l.id).collect::<Vec<_>>(), vec![3, 1]);
}

#[test]
fn each_label_carries_the_url_that_plays_it() {
    let conn = db();
    correction(&conn, 7, Some("Pippijn"), None);

    let items = list_corrections(&conn, None, 200).expect("list");

    assert_eq!(items[0].audio_url, "/api/correction/7/audio");
}

#[test]
fn the_per_speaker_tally_excludes_hidden_labels_too() {
    // The progress strip must count the same set the list shows, or the page
    // says "12 for Alex" over a list of 11.
    let conn = db();
    correction(&conn, 1, Some("Alex"), None);
    correction(&conn, 2, Some("Alex"), Some("mistaken"));
    correction(&conn, 3, None, None);

    let tally = corrections_by_speaker(&conn).expect("tally");

    assert_eq!(tally.get("Alex"), Some(&1));
    assert_eq!(
        tally.get(""),
        Some(&1),
        "unattributed counts under an empty key"
    );
}

#[test]
fn a_label_plays_its_exact_cut_by_default_and_pads_only_on_request() {
    // ⚠ The INVERSE of a turn's playback, deliberately. The Labels page exists to
    // AUDIT the cut: if the span is wrong, padding it hides the very defect you
    // opened the page to see. Context is opt-in, for when a voice cannot be
    // recognised from the trimmed fragment.
    let (exact_start, exact_end) = correction_window(10.0, 12.0, false);
    assert!((exact_start - 10.0).abs() < 1e-9);
    assert!((exact_end - 12.0).abs() < 1e-9);

    let (padded_start, padded_end) = correction_window(10.0, 12.0, true);
    assert!(padded_start < 10.0 && padded_end > 12.0);
    assert!((padded_end - padded_start) >= 5.0, "context has a floor");
}

#[test]
fn an_exact_window_never_starts_before_the_file() {
    // A fragment at the very start of a recording: a negative -ss makes ffmpeg
    // fail rather than clamp.
    let (start, _) = correction_window(-0.4, 1.0, false);
    assert!(start >= 0.0);
}
