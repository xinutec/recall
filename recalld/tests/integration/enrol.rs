//! Voiceprint enrolment's work-list (stage E4, #1538): which named turns become
//! reference vectors, and which clip each is cut from.

use chrono::{DateTime, Utc};
use recalld::enrol::{Span, derive_jobs, pending, spans_for};
use recalld::queue::{ENROLL_SPEAKER, ensure_schema};
use recalld::store;

fn at(iso: &str) -> DateTime<Utc> {
    iso.parse().expect("t")
}

/// The meaning plane's shape, in its own timestamp spelling (`+00:00`).
fn meaning() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().expect("mem");
    conn.execute_batch(
        "CREATE TABLE audio_segments (
             id INTEGER PRIMARY KEY, source_id TEXT NOT NULL, path TEXT NOT NULL,
             start_utc TEXT NOT NULL);
         CREATE TABLE transcript_segments (
             id INTEGER PRIMARY KEY, audio_segment_id INTEGER, start_utc TEXT NOT NULL,
             end_utc TEXT NOT NULL, speaker_label TEXT, superseded_by INTEGER,
             hidden_reason TEXT);
         CREATE TABLE speaker_embeddings (
             id INTEGER PRIMARY KEY, speaker_id INTEGER, vector TEXT NOT NULL,
             created_utc TEXT NOT NULL, source_correction_id INTEGER,
             source_segment_id INTEGER);
         INSERT INTO audio_segments (id, source_id, path, start_utc) VALUES
             (1, 'usb', '/data/usb/usb-20260910T100000.opus', '2026-09-10T10:00:00+00:00');",
    )
    .expect("schema");
    conn
}

/// A named turn `offset` seconds into clip 1, lasting `secs`.
fn turn(conn: &rusqlite::Connection, id: i64, label: Option<&str>, offset: f64, secs: f64) {
    let base = at("2026-09-10T10:00:00+00:00");
    let start = base + chrono::Duration::milliseconds((offset * 1000.0) as i64);
    let end = start + chrono::Duration::milliseconds((secs * 1000.0) as i64);
    conn.execute(
        "INSERT INTO transcript_segments
             (id, audio_segment_id, start_utc, end_utc, speaker_label)
         VALUES (?1, 1, ?2, ?3, ?4)",
        rusqlite::params![id, start.to_rfc3339(), end.to_rfc3339(), label],
    )
    .expect("turn");
}

/// The ingest plane's row for clip 1 — note the OTHER extension.
fn delivered(root: &std::path::Path, filename: &str) {
    let conn = store::open(root).expect("ingest");
    store::insert(
        &conn,
        &store::Row {
            source: "usb".into(),
            filename: filename.into(),
            start_utc: "2026-09-10T10:00:00Z".into(),
            bytes: 1,
            sha256: "x".into(),
            received_utc: "2026-09-10T10:01:00Z".into(),
            sent_utc: None,
        },
    )
    .expect("row");
}

#[test]
fn a_named_turn_is_offered_as_seconds_into_its_clip() {
    // ⚠ The span is relative to the CLIP, not the epoch: the runner has one file
    // and cuts inside it. An absolute instant here would seek past the end of
    // every clip in the archive.
    let conn = meaning();
    turn(&conn, 10, Some("Alice"), 12.5, 4.0);

    let work = pending(&conn).expect("pending");
    assert_eq!(
        work,
        vec![(
            "usb-20260910T100000".to_owned(),
            Span {
                segment_id: 10,
                start_s: 12.5,
                end_s: 16.5
            }
        )]
    );
}

#[test]
fn a_turn_already_enrolled_is_not_offered_again() {
    let conn = meaning();
    turn(&conn, 10, Some("Alice"), 0.0, 4.0);
    conn.execute(
        "INSERT INTO speaker_embeddings (vector, created_utc, source_segment_id)
         VALUES ('[1.0]', '2026-09-10T11:00:00+00:00', 10)",
        [],
    )
    .expect("print");

    assert!(pending(&conn).expect("pending").is_empty());
}

#[test]
fn the_work_list_matches_the_pythons_own_exclusions() {
    // ⚠ Both select the same turns while both exist. A turn one enrols and the
    // other does not would be enrolled twice, under two prints of one clip.
    let conn = meaning();
    turn(&conn, 1, None, 0.0, 4.0); // never named
    turn(&conn, 2, Some("SPEAKER_01"), 5.0, 4.0); // a cluster id is not a name
    turn(&conn, 3, Some("Alice"), 10.0, 0.5); // under a second: a useless print
    turn(&conn, 4, Some("Alice"), 20.0, 4.0); // the only one that qualifies
    conn.execute(
        "UPDATE transcript_segments SET superseded_by = 9 WHERE id = 1",
        [],
    )
    .expect("supersede");

    let ids: Vec<i64> = pending(&conn)
        .expect("pending")
        .into_iter()
        .map(|(_, s)| s.segment_id)
        .collect();
    assert_eq!(ids, vec![4]);
}

#[test]
fn a_turn_hidden_or_superseded_teaches_nothing() {
    let conn = meaning();
    turn(&conn, 1, Some("Alice"), 0.0, 4.0);
    turn(&conn, 2, Some("Alice"), 10.0, 4.0);
    conn.execute(
        "UPDATE transcript_segments SET hidden_reason = 'review' WHERE id = 1",
        [],
    )
    .expect("hide");
    conn.execute(
        "UPDATE transcript_segments SET superseded_by = 5 WHERE id = 2",
        [],
    )
    .expect("supersede");

    assert!(pending(&conn).expect("pending").is_empty());
}

#[test]
fn a_turn_starting_a_hair_before_its_clip_seeks_to_zero_not_backwards() {
    // ⚠ ffmpeg answers a negative seek with the WHOLE CLIP rather than an error,
    // so an unclamped offset enrols a minute of the room as one person's voice.
    let conn = meaning();
    turn(&conn, 10, Some("Alice"), -0.2, 4.0);

    let (_, span) = pending(&conn).expect("pending").remove(0);
    assert!(span.start_s.abs() < f64::EPSILON, "got {}", span.start_s);
}

#[test]
fn the_job_names_the_ingest_filename_even_when_the_extensions_differ() {
    // ⚠ The meaning plane holds `.opus`, the ingest copy is `.wav`. The runner
    // fetches by the INGEST name, so matching on whole filenames would derive
    // nothing and the queue would sit empty while turns waited.
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = meaning();
    turn(&conn, 10, Some("Alice"), 0.0, 4.0);
    delivered(dir.path(), "usb-20260910T100000.wav");

    let ingest = store::open(dir.path()).expect("db");
    ensure_schema(&ingest).expect("schema");
    assert_eq!(
        derive_jobs(&ingest, &conn, at("2026-09-10T12:00:00Z"), 100).expect("derive"),
        1
    );
    let queued: String = ingest
        .query_row(
            "SELECT filename FROM jobs WHERE kind = ?1",
            [ENROLL_SPEAKER],
            |r| r.get(0),
        )
        .expect("job");
    assert_eq!(queued, "usb-20260910T100000.wav");
    assert_eq!(
        spans_for(&conn, &queued).expect("spans"),
        vec![Span {
            segment_id: 10,
            start_s: 0.0,
            end_s: 4.0
        }]
    );
}

#[test]
fn one_job_per_clip_however_many_named_turns_it_holds() {
    // `jobs` is UNIQUE (kind, filename), so per-turn jobs could not exist even
    // if they were wanted. The spans travel with the lease instead.
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = meaning();
    turn(&conn, 10, Some("Alice"), 0.0, 4.0);
    turn(&conn, 11, Some("Bob"), 10.0, 4.0);
    delivered(dir.path(), "usb-20260910T100000.wav");

    let ingest = store::open(dir.path()).expect("db");
    ensure_schema(&ingest).expect("schema");
    assert_eq!(
        derive_jobs(&ingest, &conn, at("2026-09-10T12:00:00Z"), 100).expect("derive"),
        1
    );
    assert_eq!(
        spans_for(&conn, "usb-20260910T100000.wav")
            .expect("spans")
            .len(),
        2
    );
    // Idempotent: a second pass adds nothing.
    assert_eq!(
        derive_jobs(&ingest, &conn, at("2026-09-10T12:05:00Z"), 100).expect("derive"),
        0
    );
}

#[test]
fn a_turn_whose_clip_was_never_delivered_gets_no_job() {
    // The runner fetches from the ingest plane. A job for audio that is not
    // there could only burn its attempts.
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = meaning();
    turn(&conn, 10, Some("Alice"), 0.0, 4.0);

    let ingest = store::open(dir.path()).expect("db");
    ensure_schema(&ingest).expect("schema");
    assert_eq!(
        derive_jobs(&ingest, &conn, at("2026-09-10T12:00:00Z"), 100).expect("derive"),
        0
    );
}
