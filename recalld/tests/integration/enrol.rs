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
const SCHEMA: &str = "CREATE TABLE audio_segments (
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
             (1, 'usb', '/data/usb/usb-20260910T100000.opus', '2026-09-10T10:00:00+00:00');";

fn meaning() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().expect("mem");
    conn.execute_batch(SCHEMA).expect("schema");
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

// --- writing back what the runner embedded -----------------------------------

use recalld::enrol::{Print, write_pass};

/// A finished `enroll-speaker` job carrying `result`.
fn finished(ingest: &rusqlite::Connection, filename: &str, result: &str) {
    ingest
        .execute(
            "INSERT INTO jobs (kind, filename, state, created_utc, done_utc, result)
             VALUES (?1, ?2, 'done', '2026-09-10T12:00:00Z', '2026-09-10T12:05:00Z', ?3)",
            rusqlite::params![ENROLL_SPEAKER, filename, result],
        )
        .expect("job");
}

fn reply(prints: &[Print]) -> String {
    serde_json::json!({ "ok": true, "result": { "prints": prints } }).to_string()
}

fn ingest_at(root: &std::path::Path) -> rusqlite::Connection {
    let conn = store::open(root).expect("ingest");
    ensure_schema(&conn).expect("schema");
    conn
}

fn enrolled_names(conn: &rusqlite::Connection) -> Vec<(String, i64)> {
    let mut stmt = conn
        .prepare(
            "SELECT s.name, e.source_segment_id FROM speaker_embeddings e
             JOIN speakers s ON s.id = e.speaker_id ORDER BY e.id",
        )
        .expect("prepare");
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("rows");
    rows.collect::<Result<_, _>>().expect("collect")
}

#[test]
fn an_embedded_span_becomes_a_reference_voiceprint() {
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = meaning();
    conn.execute_batch("CREATE TABLE speakers (id INTEGER PRIMARY KEY, name TEXT UNIQUE);")
        .expect("speakers");
    turn(&conn, 10, Some("Alice"), 0.0, 4.0);
    let ingest = ingest_at(dir.path());
    finished(
        &ingest,
        "usb-20260910T100000.wav",
        &reply(&[Print {
            segment_id: 10,
            vector: vec![0.5, -0.25],
        }]),
    );

    let pass = write_pass(&conn, &ingest, "2026-09-10T12:10:00+00:00", 10).expect("pass");
    assert_eq!(pass.prints, 1);
    assert_eq!(enrolled_names(&conn), vec![("Alice".to_owned(), 10)]);
    // The turn is enrolled, so it leaves the work-list by itself.
    assert!(pending(&conn).expect("pending").is_empty());
}

#[test]
fn a_turn_renamed_while_the_runner_worked_is_not_filed_under_the_old_name() {
    // ⚠ Embedding takes minutes and a person can re-assign in that window. The
    // job carries no name for exactly this reason: the label is read here.
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = meaning();
    conn.execute_batch("CREATE TABLE speakers (id INTEGER PRIMARY KEY, name TEXT UNIQUE);")
        .expect("speakers");
    turn(&conn, 10, Some("Alice"), 0.0, 4.0);
    let ingest = ingest_at(dir.path());
    finished(
        &ingest,
        "usb-20260910T100000.wav",
        &reply(&[Print {
            segment_id: 10,
            vector: vec![1.0],
        }]),
    );
    conn.execute(
        "UPDATE transcript_segments SET speaker_label = 'Bob' WHERE id = 10",
        [],
    )
    .expect("rename");

    write_pass(&conn, &ingest, "2026-09-10T12:10:00+00:00", 10).expect("pass");
    assert_eq!(enrolled_names(&conn), vec![("Bob".to_owned(), 10)]);
}

#[test]
fn a_turn_hidden_while_the_runner_worked_enrols_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = meaning();
    conn.execute_batch("CREATE TABLE speakers (id INTEGER PRIMARY KEY, name TEXT UNIQUE);")
        .expect("speakers");
    turn(&conn, 10, Some("Alice"), 0.0, 4.0);
    let ingest = ingest_at(dir.path());
    finished(
        &ingest,
        "usb-20260910T100000.wav",
        &reply(&[Print {
            segment_id: 10,
            vector: vec![1.0],
        }]),
    );
    conn.execute(
        "UPDATE transcript_segments SET hidden_reason = 'review' WHERE id = 10",
        [],
    )
    .expect("hide");

    let pass = write_pass(&conn, &ingest, "2026-09-10T12:10:00+00:00", 10).expect("pass");
    assert_eq!((pass.prints, pass.stale), (0, 1));
    assert!(enrolled_names(&conn).is_empty());
}

#[test]
fn an_empty_vector_is_refused_rather_than_enrolled() {
    // ⚠ A zero-length vector is not inert. `identify::enrolled` skips a row it
    // cannot parse for the same reason: a degenerate print sits at cosine 0
    // against everyone and becomes somebody's best match on quiet audio.
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = meaning();
    conn.execute_batch("CREATE TABLE speakers (id INTEGER PRIMARY KEY, name TEXT UNIQUE);")
        .expect("speakers");
    turn(&conn, 10, Some("Alice"), 0.0, 4.0);
    let ingest = ingest_at(dir.path());
    finished(
        &ingest,
        "usb-20260910T100000.wav",
        &reply(&[Print {
            segment_id: 10,
            vector: vec![],
        }]),
    );

    let pass = write_pass(&conn, &ingest, "2026-09-10T12:10:00+00:00", 10).expect("pass");
    assert_eq!((pass.prints, pass.stale), (0, 1));
    assert!(enrolled_names(&conn).is_empty());
}

#[test]
fn a_clip_that_enrols_nothing_is_still_ledgered() {
    // ⚠ The candidate query is "not in the ledger". A decision that writes no
    // row would leave the clip a candidate for ever, re-deciding it every pass.
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = meaning();
    conn.execute_batch("CREATE TABLE speakers (id INTEGER PRIMARY KEY, name TEXT UNIQUE);")
        .expect("speakers");
    let ingest = ingest_at(dir.path());
    finished(&ingest, "usb-20260910T100000.wav", "not json at all");

    let first = write_pass(&conn, &ingest, "2026-09-10T12:10:00+00:00", 10).expect("pass");
    assert_eq!(first.clips, 1);
    let again = write_pass(&conn, &ingest, "2026-09-10T12:20:00+00:00", 10).expect("pass");
    assert_eq!(again.clips, 0, "a decided clip must not come back");
}

#[test]
fn a_replayed_result_does_not_enrol_the_same_turn_twice() {
    // Belt as well as braces: the ledger stops the clip returning, and
    // `still_wanted` stops the turn being enrolled again even if it did.
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = meaning();
    conn.execute_batch("CREATE TABLE speakers (id INTEGER PRIMARY KEY, name TEXT UNIQUE);")
        .expect("speakers");
    turn(&conn, 10, Some("Alice"), 0.0, 4.0);
    let ingest = ingest_at(dir.path());
    let payload = reply(&[Print {
        segment_id: 10,
        vector: vec![1.0],
    }]);
    finished(&ingest, "usb-20260910T100000.wav", &payload);
    write_pass(&conn, &ingest, "2026-09-10T12:10:00+00:00", 10).expect("pass");

    ingest
        .execute("DELETE FROM pass_ledger", [])
        .expect("forget the ledger");
    let pass = write_pass(&conn, &ingest, "2026-09-10T12:20:00+00:00", 10).expect("pass");
    assert_eq!((pass.prints, pass.stale), (0, 1));
    assert_eq!(enrolled_names(&conn).len(), 1);
}

#[test]
fn a_leased_enrolment_job_carries_its_spans_and_other_kinds_carry_none() {
    // ⚠ The runner has one clip and no way to ask which stretches to embed, so a
    // job that arrives without spans embeds nothing and enrols nobody — silently,
    // because an empty print list is a valid result. Other kinds must stay
    // byte-identical on the wire: `spans` is skipped when empty.
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let conn =
            rusqlite::Connection::open(dir.path().join("recall.sqlite")).expect("meaning file");
        conn.execute_batch(SCHEMA).expect("schema");
        turn(&conn, 10, Some("Alice"), 2.0, 4.0);
    }

    let mut enrol_job = recalld::queue::Job {
        id: 1,
        kind: ENROLL_SPEAKER.to_owned(),
        filename: "usb-20260910T100000.wav".to_owned(),
        source: "usb".to_owned(),
        spans: Vec::new(),
    };
    recalld::enrol::attach_spans(dir.path(), &mut enrol_job).expect("attach");
    assert_eq!(
        enrol_job.spans,
        vec![Span {
            segment_id: 10,
            start_s: 2.0,
            end_s: 6.0
        }]
    );
    assert!(
        serde_json::to_string(&enrol_job)
            .expect("json")
            .contains("spans")
    );

    let mut diarize_job = recalld::queue::Job {
        kind: "diarize-segment".to_owned(),
        spans: Vec::new(),
        ..enrol_job.clone()
    };
    recalld::enrol::attach_spans(dir.path(), &mut diarize_job).expect("attach");
    assert!(diarize_job.spans.is_empty());
    assert!(
        !serde_json::to_string(&diarize_job)
            .expect("json")
            .contains("spans"),
        "an unrelated kind's lease body must not grow a field"
    );
}

#[test]
fn a_lease_of_another_kind_does_not_need_the_meaning_plane_at_all() {
    // ⚠ Regression: opening `recall.sqlite` unconditionally made EVERY lease 500
    // wherever it was absent, which is the runner's own end-to-end harness. A
    // transcription runner must not be stopped by a database it never reads.
    let empty = tempfile::tempdir().expect("tempdir");
    let mut job = recalld::queue::Job {
        id: 1,
        kind: "transcribe-segment".to_owned(),
        filename: "usb-20260910T100000.wav".to_owned(),
        source: "usb".to_owned(),
        spans: Vec::new(),
    };
    recalld::enrol::attach_spans(empty.path(), &mut job).expect("no meaning plane, no problem");
}
