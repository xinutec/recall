//! Voiceprint enrolment's work-list: which named turns become reference
//! vectors, and which clip each is cut from.

use audiocore::job::Kind;
use chrono::{DateTime, Utc};
use recalld::enrol::{Span, derive_jobs, pending, spans_for};
use recalld::store;

fn at(iso: &str) -> DateTime<Utc> {
    iso.parse().expect("t")
}

/// The meaning plane with clip 1 and one enrolled speaker, in the stored spelling.
fn seed(conn: &rusqlite::Connection) {
    recalld::meaning_schema::ensure(conn).expect("schema");
    conn.execute_batch(
        "INSERT INTO sources (id, name, kind) VALUES ('usb', 'usb', 'coreaudio');
         INSERT INTO speakers (id, name) VALUES (1, 'Someone');
         INSERT INTO audio_segments (id, source_id, path, start_utc, end_utc, sample_rate, channels)
         VALUES (1, 'usb', '/data/usb/usb-20260910T100000.opus', '2026-09-10T10:00:00+00:00',
                 '2026-09-10T10:01:00+00:00', 16000, 1);",
    )
    .expect("clip 1");
}

fn meaning() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().expect("mem");
    seed(&conn);
    conn
}

/// A named turn `offset` seconds into clip 1, lasting `secs`.
fn turn(conn: &rusqlite::Connection, id: i64, label: Option<&str>, offset: f64, secs: f64) {
    let base = at("2026-09-10T10:00:00+00:00");
    let start = base + chrono::Duration::milliseconds((offset * 1000.0) as i64);
    let end = start + chrono::Duration::milliseconds((secs * 1000.0) as i64);
    conn.execute(
        "INSERT INTO transcript_segments
             (id, audio_segment_id, start_utc, end_utc, text, asr_model, speaker_label)
         VALUES (?1, 1, ?2, ?3, 'x', 'whisper', ?4)",
        rusqlite::params![
            id,
            audiocore::instant::python_isoformat_utc(start),
            audiocore::instant::python_isoformat_utc(end),
            label
        ],
    )
    .expect("turn");
}

/// The ingest plane's row for clip 1 — note the OTHER extension.
fn delivered(root: &std::path::Path, filename: &str) {
    delivered_at(root, filename, "2026-09-10T10:00:00Z");
}

/// A delivered blob with a capture time of its own.
fn delivered_at(root: &std::path::Path, filename: &str, start_utc: &str) {
    let conn = store::open(root).expect("ingest");
    store::insert(
        &conn,
        &store::Row {
            source: "usb".into(),
            filename: filename.into(),
            start_utc: start_utc.into(),
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
    // The span is relative to the clip, not the epoch: the runner has one file
    // and cuts inside it.
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
        "INSERT INTO speaker_embeddings (speaker_id, vector, created_utc, source_segment_id)
         VALUES (1, '[1.0]', '2026-09-10T11:00:00+00:00', 10)",
        [],
    )
    .expect("print");

    assert!(pending(&conn).expect("pending").is_empty());
}

#[test]
fn the_work_list_matches_the_pythons_own_exclusions() {
    // The same exclusions as the Python enrolment this ports, so a turn it
    // enrolled is not enrolled again under a second print of one clip.
    let conn = meaning();
    turn(&conn, 1, None, 0.0, 4.0); // never named
    turn(&conn, 2, Some("SPEAKER_01"), 5.0, 4.0); // a cluster id is not a name
    turn(&conn, 3, Some("Alice"), 10.0, 0.5); // under a second: a useless print
    turn(&conn, 4, Some("Alice"), 20.0, 4.0); // the only one that qualifies
    conn.execute(
        "UPDATE transcript_segments SET superseded_by = 4 WHERE id = 1",
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
        "UPDATE transcript_segments SET superseded_by = 1 WHERE id = 2",
        [],
    )
    .expect("supersede");

    assert!(pending(&conn).expect("pending").is_empty());
}

#[test]
fn a_turn_starting_a_hair_before_its_clip_seeks_to_zero_not_backwards() {
    // ⚠ ffmpeg answers a negative seek with the whole clip rather than an error,
    // so an unclamped offset enrols a minute of the room as one person's voice.
    let conn = meaning();
    turn(&conn, 10, Some("Alice"), -0.2, 4.0);

    let (_, span) = pending(&conn).expect("pending").remove(0);
    assert!(span.start_s.abs() < f64::EPSILON, "got {}", span.start_s);
}

#[test]
fn the_job_names_the_ingest_filename_even_when_the_extensions_differ() {
    // ⚠ The meaning plane holds `.opus`, the ingest copy is `.wav`, and the
    // runner fetches by the ingest name: matching whole filenames derives nothing.
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = meaning();
    turn(&conn, 10, Some("Alice"), 0.0, 4.0);
    delivered(dir.path(), "usb-20260910T100000.wav");

    let ingest = store::open(dir.path()).expect("db");
    recalld::ingest_schema::ensure(&ingest).expect("schema");
    assert_eq!(
        derive_jobs(&ingest, &conn, at("2026-09-10T12:00:00Z"), 100).expect("derive"),
        1
    );
    let queued: String = ingest
        .query_row(
            "SELECT filename FROM jobs WHERE kind = ?1",
            [Kind::EnrollSpeaker],
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
    recalld::ingest_schema::ensure(&ingest).expect("schema");
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
    recalld::ingest_schema::ensure(&ingest).expect("schema");
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
            rusqlite::params![Kind::EnrollSpeaker, filename, result],
        )
        .expect("job");
}

fn reply(prints: &[Print]) -> String {
    serde_json::json!({ "ok": true, "result": { "prints": prints } }).to_string()
}

fn ingest_at(root: &std::path::Path) -> rusqlite::Connection {
    let conn = store::open(root).expect("ingest");
    recalld::ingest_schema::ensure(&conn).expect("schema");
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
    // Embedding takes minutes and a person can re-assign in that window, so the
    // job carries no name: the label is read here.
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = meaning();
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
    // A zero-length vector is not inert: a degenerate print sits at cosine 0
    // against everyone and becomes somebody's best match on quiet audio.
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = meaning();
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
    // The candidate query is "not in the ledger", so a decision that wrote no
    // row would be re-made every pass.
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = meaning();
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
    // The runner cannot ask which stretches to embed, so a job without spans
    // enrols nobody, silently: an empty print list is a valid result. Other kinds
    // stay byte-identical on the wire because `spans` is skipped when empty.
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let conn =
            rusqlite::Connection::open(dir.path().join("recall.sqlite")).expect("meaning file");
        seed(&conn);
        turn(&conn, 10, Some("Alice"), 2.0, 4.0);
    }

    let mut enrol_job = recalld::queue::Job {
        id: 1,
        kind: Kind::EnrollSpeaker,
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
        kind: Kind::DiarizeSegment,
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
    // A lease of another kind must not need `recall.sqlite`: the runner's own
    // end-to-end harness has none, and a transcription runner must not be stopped
    // by a database it never reads.
    let empty = tempfile::tempdir().expect("tempdir");
    let mut job = recalld::queue::Job {
        id: 1,
        kind: Kind::TranscribeSegment,
        filename: "usb-20260910T100000.wav".to_owned(),
        source: "usb".to_owned(),
        spans: Vec::new(),
    };
    recalld::enrol::attach_spans(empty.path(), &mut job).expect("no meaning plane, no problem");
}

#[test]
fn enrolment_outranks_capture_time_or_it_would_never_be_leased() {
    // Clips awaiting a print are old (a label lands on whatever clip a person is
    // reading), so ordered by capture time they would wait behind days of newer
    // diarize jobs.
    let dir = tempfile::tempdir().expect("tempdir");
    let ingest = ingest_at(dir.path());
    // A NEWER clip with diarization to do, and an OLDER one awaiting a print.
    delivered_at(
        dir.path(),
        "usb-20260912T100000.wav",
        "2026-09-12T10:00:00Z",
    );
    delivered(dir.path(), "usb-20260910T100000.wav");
    for (kind, filename) in [
        (Kind::DiarizeSegment, "usb-20260912T100000.wav"),
        (Kind::EnrollSpeaker, "usb-20260910T100000.wav"),
    ] {
        ingest
            .execute(
                "INSERT INTO jobs (kind, filename, created_utc)
                 VALUES (?1, ?2, '2026-09-12T11:00:00Z')",
                rusqlite::params![kind, filename],
            )
            .expect("job");
    }

    let job = recalld::queue::lease(
        dir.path(),
        at("2026-09-12T12:00:00Z"),
        &[Kind::DiarizeSegment, Kind::EnrollSpeaker],
    )
    .expect("lease")
    .expect("a job");
    assert_eq!(
        job.kind,
        Kind::EnrollSpeaker,
        "the older clip's print comes first"
    );
}
