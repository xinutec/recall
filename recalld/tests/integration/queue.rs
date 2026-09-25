//! The job queue: derive, lease newest first, done; a lapsed lease re-offers.

use audiocore::job::Kind;
use chrono::{DateTime, Duration, Utc};
use recalld::queue::{done, lease};
use recalld::store;

/// A room block from when the room stream ran in production: history now.
fn room_row(root: &std::path::Path, stamp: &str) {
    let conn = store::open(root).expect("db");
    store::insert(
        &conn,
        &store::Row {
            source: "room".into(),
            filename: format!("room-{stamp}.flac"),
            start_utc: stamp.into(),
            bytes: 1,
            sha256: "x".into(),
            received_utc: "2026-09-05T00:00:00Z".into(),
            sent_utc: None,
        },
    )
    .expect("row");
}

/// A usb clip with its transcription job queued, as `derive_segment_jobs` leaves it.
fn queued(root: &std::path::Path, stamp: &str) -> String {
    let name = mic_row(root, "usb", stamp);
    store::open(root)
        .expect("db")
        .execute(
            "INSERT INTO jobs (kind, filename, created_utc) VALUES (?1, ?2, '2026-09-05T00:00:00Z')",
            (Kind::TranscribeSegment, &name),
        )
        .expect("job");
    name
}

const ASR: &[Kind] = &[Kind::TranscribeSegment];
const VOICES: &[Kind] = &[Kind::DiarizeSegment];

#[test]
fn newest_first_lease_done_and_lapse() {
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-05T12:00:00Z".parse().expect("t");
    let older = queued(dir.path(), "20260905T100000");
    let newer = queued(dir.path(), "20260905T110000");
    // Newest first.
    let first = lease(dir.path(), now, ASR).expect("lease").expect("job");
    assert_eq!(first.filename, newer);
    // The leased job is not re-offered while its lease holds…
    let second = lease(dir.path(), now, ASR).expect("lease").expect("job");
    assert_eq!(second.filename, older);
    assert!(lease(dir.path(), now, ASR).expect("lease").is_none());
    // …but a lapsed lease re-offers, and done retires for good.
    let later = now + Duration::minutes(20);
    let again = lease(dir.path(), later, ASR).expect("lease").expect("job");
    assert_eq!(again.filename, newer);
    assert!(done(dir.path(), again.id, "{}", later).expect("done"));
    assert!(!done(dir.path(), again.id, "{}", later).expect("idempotent"));
    let last = lease(dir.path(), later + Duration::minutes(20), ASR)
        .expect("lease")
        .expect("job");
    assert_eq!(last.filename, older);
}

#[test]
fn a_job_nobody_finishes_is_retired_after_its_attempts_are_spent() {
    // A clip that crashes the shim never reaches `done`, so without a cap it is
    // re-offered for ever. After the cap it is recorded as a failure.
    let dir = tempfile::tempdir().expect("tempdir");
    let start: DateTime<Utc> = "2026-09-05T12:00:00Z".parse().expect("t");
    let name = queued(dir.path(), "20260905T100000");
    let mut now = start;
    for attempt in 1..=queue::MAX_ATTEMPTS {
        let job = lease(dir.path(), now, ASR)
            .expect("lease")
            .unwrap_or_else(|| panic!("attempt {attempt} must still be offered"));
        assert_eq!(job.filename, name);
        now += Duration::minutes(20);
    }
    assert!(
        lease(dir.path(), now, ASR).expect("lease").is_none(),
        "spent: not offered again"
    );
    let (state, result): (String, String) = store::open(dir.path())
        .expect("db")
        .query_row(
            "SELECT state, result FROM jobs WHERE filename = ?1",
            [&name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("row");
    assert_eq!(state, "done");
    assert!(
        result.contains(r#""ok":false"#),
        "recorded as a failure: {result}"
    );
}

#[test]
fn a_segment_measured_as_silent_gets_no_transcription_job() {
    // Transcribing silence returns invented text, not nothing.
    let dir = tempfile::tempdir().expect("tempdir");
    let silent = mic_row(dir.path(), "usb", "20260906T100000");
    let speech = mic_row(dir.path(), "usb", "20260906T100100");
    let unmeasured = mic_row(dir.path(), "usb", "20260906T100200");
    let ingest = store::open(dir.path()).expect("db");
    for (name, seconds) in [(&silent, 0.0), (&speech, 12.0)] {
        ingest
            .execute(
                "INSERT INTO segment_speech (filename, source, speech_seconds, computed_utc)
                 VALUES (?1, 'usb', ?2, '2026-09-06T10:01:00Z')",
                (name, seconds),
            )
            .expect("speech row");
    }
    let now: DateTime<Utc> = "2026-09-06T10:30:00Z".parse().expect("t");
    derive_segment_jobs(&ingest, &meaning_plane(), now, 100).expect("derive");
    let mut queued: Vec<String> = ingest
        .prepare("SELECT filename FROM jobs")
        .expect("prep")
        .query_map([], |r| r.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    queued.sort();
    // Unmeasured is queued too: not yet measured is not silent.
    assert_eq!(
        queued,
        vec![speech, unmeasured],
        "the SILENT segment must not be queued"
    );
}

/// The ASR shim's result envelope, trimmed to what the derivation reads.
const TRANSCRIBED: &str = r#"{"ok":true,"result":{"language":"nl","segments":[]}}"#;
const REFUSED: &str = r#"{"ok":false,"error":"FileNotFoundError: /x"}"#;

fn kinds_queued(root: &std::path::Path) -> Vec<(String, String)> {
    let conn = store::open(root).expect("db");
    let mut stmt = conn
        .prepare("SELECT kind, filename FROM jobs ORDER BY kind, filename")
        .expect("prep");
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows")
}

#[test]
fn a_diarize_job_appears_only_once_the_words_exist() {
    // Diarization spans become turns only when aligned against words, so the
    // job derives from a succeeded transcription, never from the segment.
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-11T12:00:00Z".parse().expect("t");
    let name = queued(dir.path(), "20260911T100000");

    let job = lease(dir.path(), now, ASR).expect("lease").expect("job");
    assert!(
        !kinds_queued(dir.path())
            .iter()
            .any(|(kind, _)| kind == Kind::DiarizeSegment.as_str()),
        "no diarize job before the clip is transcribed"
    );

    assert!(done(dir.path(), job.id, TRANSCRIBED, now).expect("done"));
    let next = lease(dir.path(), now, VOICES).expect("lease").expect("job");
    assert_eq!(next.kind, Kind::DiarizeSegment);
    assert_eq!(next.filename, name);
}

#[test]
fn a_refused_transcription_derives_no_diarization() {
    // A refused clip is the problem (`turns::Barren::Refused`); diarizing it
    // would spend GPU to learn that again.
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-11T12:00:00Z".parse().expect("t");
    queued(dir.path(), "20260911T100000");
    let job = lease(dir.path(), now, ASR).expect("lease").expect("job");
    assert!(done(dir.path(), job.id, REFUSED, now).expect("done"));

    assert!(
        lease(dir.path(), now, VOICES).expect("lease").is_none(),
        "a refused clip must not be queued for diarization"
    );
}

#[test]
fn a_runner_is_never_handed_a_kind_it_cannot_do() {
    // A runner holds one shim's weights; another kind would burn the job's
    // attempts.
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-11T12:00:00Z".parse().expect("t");
    queued(dir.path(), "20260911T100000");
    let job = lease(dir.path(), now, ASR).expect("lease").expect("job");
    assert!(done(dir.path(), job.id, TRANSCRIBED, now).expect("done"));

    // Only a diarize job is now outstanding, and an asr-only runner sees nothing.
    assert!(lease(dir.path(), now, ASR).expect("lease").is_none());
    // An empty capability list leases nothing, not everything.
    assert!(lease(dir.path(), now, &[]).expect("lease").is_none());
    // A runner that can do both takes it.
    let both = lease(
        dir.path(),
        now,
        &[Kind::TranscribeSegment, Kind::DiarizeSegment],
    )
    .expect("lease")
    .expect("job");
    assert_eq!(both.kind, Kind::DiarizeSegment);
}

#[test]
fn the_retired_room_streams_open_jobs_are_closed_not_deleted() {
    // Production queued a room job every minute; nothing takes them now.
    let dir = tempfile::tempdir().expect("tempdir");
    room_row(dir.path(), "20260905T100000");
    let conn = store::open(dir.path()).expect("db");
    conn.execute_batch(
        "INSERT INTO jobs (kind, filename, created_utc) VALUES
             ('transcribe-room', 'room-20260905T100000.flac', '2026-09-05T10:01:00Z'),
             ('diarize-room', 'room-20260905T100000.flac', '2026-09-05T10:02:00Z');",
    )
    .expect("jobs");
    recalld::ingest_schema::ensure(&conn).expect("reopen");
    let rows: Vec<(String, String)> = conn
        .prepare("SELECT state, result FROM jobs ORDER BY kind")
        .expect("prep")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    assert_eq!(rows.len(), 2, "kept as history");
    for (state, result) in rows {
        assert_eq!(state, "done");
        assert!(result.contains("room stream retired"), "{result}");
    }
}

// ---- per-mic transcribe jobs ----

use recalld::queue::{self, derive_segment_jobs};

fn mic_row(root: &std::path::Path, source: &str, stamp: &str) -> String {
    let name = format!("{source}-{stamp}.opus");
    let conn = store::open(root).expect("db");
    store::insert(
        &conn,
        &store::Row {
            source: source.into(),
            filename: name.clone(),
            // The ingest plane's spelling: a trailing Z.
            start_utc: format!(
                "{}-{}-{}T{}:{}:{}Z",
                &stamp[0..4],
                &stamp[4..6],
                &stamp[6..8],
                &stamp[9..11],
                &stamp[11..13],
                &stamp[13..15]
            ),
            bytes: 1,
            sha256: "x".into(),
            received_utc: "2026-09-05T00:00:00Z".into(),
            sent_utc: None,
        },
    )
    .expect("row");
    name
}

/// The meaning plane `derive_segment_jobs` reads, with three sources.
fn meaning_plane() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().expect("mem");
    recalld::meaning_schema::ensure(&conn).expect("schema");
    conn.execute_batch(
        "INSERT INTO sources (id, name, kind) VALUES
             ('usb', 'usb', 'coreaudio'),
             ('geb', 'geb', 'tcp_pcm'),
             ('room', 'Room', 'derived');",
    )
    .expect("schema");
    conn
}

/// Register a segment as transcribed, in the meaning plane's own spellings.
fn already_transcribed(meaning: &rusqlite::Connection, source: &str, filename: &str, start: &str) {
    meaning
        .execute(
            "INSERT INTO audio_segments (source_id, path, start_utc, end_utc, sample_rate, channels)
             VALUES (?1, ?2, ?3, ?3, 16000, 1)",
            (source, format!("/data/{source}/{filename}"), start),
        )
        .expect("audio");
    let id = meaning.last_insert_rowid();
    meaning
        .execute(
            "INSERT INTO transcript_segments (audio_segment_id, start_utc, end_utc, text, asr_model)
             VALUES (?1, ?2, ?2, 'some words', 'whisper')",
            (id, start),
        )
        .expect("turn");
}

#[test]
fn a_segment_that_already_has_turns_gets_no_job() {
    // ⚠ The join key is the filename. Joining on start_utc matches nothing: the
    // planes spell the same instant `...Z` and `...+00:00`, and text comparison
    // returns a confident zero rather than an error.
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-05T12:00:00Z".parse().expect("t");
    let done_one = mic_row(dir.path(), "usb", "20260905T100000");
    let todo = mic_row(dir.path(), "usb", "20260905T110000");

    let meaning = meaning_plane();
    already_transcribed(&meaning, "usb", &done_one, "2026-09-05T10:00:00+00:00");

    let ingest = store::open(dir.path()).expect("db");
    recalld::ingest_schema::ensure(&ingest).expect("schema");
    assert_eq!(
        derive_segment_jobs(&ingest, &meaning, now, 100).expect("derive"),
        1,
        "exactly the untranscribed one"
    );
    let queued: String = ingest
        .query_row(
            "SELECT filename FROM jobs WHERE kind = ?1",
            [Kind::TranscribeSegment],
            |r| r.get(0),
        )
        .expect("job");
    assert_eq!(
        queued, todo,
        "the transcribed segment must not be re-queued"
    );
}

#[test]
fn room_blocks_are_not_derived_as_per_mic_work() {
    // The room stream's old blocks stay in the store as history, and are no
    // microphone's clips.
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-05T12:00:00Z".parse().expect("t");
    room_row(dir.path(), "20260905T100000");
    let meaning = meaning_plane();
    let ingest = store::open(dir.path()).expect("db");
    recalld::ingest_schema::ensure(&ingest).expect("schema");
    assert_eq!(
        derive_segment_jobs(&ingest, &meaning, now, 100).expect("derive"),
        0
    );
}

#[test]
fn the_derivation_is_bounded_and_newest_first() {
    // A backlog derived in one statement would queue days of GPU work at once,
    // competing with the room stream. The bound keeps it reversible.
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-05T12:00:00Z".parse().expect("t");
    for minute in 0..5 {
        mic_row(dir.path(), "usb", &format!("20260905T10{minute:02}00"));
    }
    let meaning = meaning_plane();
    let ingest = store::open(dir.path()).expect("db");
    recalld::ingest_schema::ensure(&ingest).expect("schema");
    assert_eq!(
        derive_segment_jobs(&ingest, &meaning, now, 2).expect("derive"),
        2,
        "the limit bounds the work queued"
    );
    let newest: String = ingest
        .query_row(
            "SELECT filename FROM jobs WHERE kind = ?1 ORDER BY filename DESC LIMIT 1",
            [Kind::TranscribeSegment],
            |r| r.get(0),
        )
        .expect("job");
    assert_eq!(
        newest, "usb-20260905T100400.opus",
        "newest first — what they are saying now outranks backfill"
    );
}

#[test]
fn deriving_twice_queues_nothing_new() {
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-05T12:00:00Z".parse().expect("t");
    mic_row(dir.path(), "usb", "20260905T100000");
    let meaning = meaning_plane();
    let ingest = store::open(dir.path()).expect("db");
    recalld::ingest_schema::ensure(&ingest).expect("schema");
    assert_eq!(
        derive_segment_jobs(&ingest, &meaning, now, 100).expect("a"),
        1
    );
    assert_eq!(
        derive_segment_jobs(&ingest, &meaning, now, 100).expect("b"),
        0,
        "derivation is idempotent — a job already queued is not queued again"
    );
}

#[test]
fn an_uploaded_meeting_is_leased_by_the_same_runner_as_a_microphone() {
    // Uploads have no other transcriber: nothing transcribes them on arrival,
    // so the ingest plane's queue is their only road.
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-08T12:00:00Z".parse().expect("t");
    let meeting = mic_row(dir.path(), "meeting-20260907-0905", "20260907T090500");
    mic_row(dir.path(), "usb", "20260907T100000");

    let meaning = meaning_plane();
    meaning
        .execute(
            "INSERT INTO sources (id, name, kind)
             VALUES ('meeting-20260907-0905', 'Meeting', 'upload')",
            [],
        )
        .expect("meeting source");

    let ingest = store::open(dir.path()).expect("db");
    recalld::ingest_schema::ensure(&ingest).expect("schema");
    assert_eq!(
        derive_segment_jobs(&ingest, &meaning, now, 100).expect("derive"),
        2,
        "the microphone clip AND the meeting"
    );
    let queued: Vec<String> = {
        let mut stmt = ingest
            .prepare("SELECT filename FROM jobs WHERE kind = ?1 ORDER BY filename")
            .expect("prepare");
        let rows = stmt
            .query_map([Kind::TranscribeSegment], |r| r.get::<_, String>(0))
            .expect("rows");
        rows.collect::<Result<_, _>>().expect("collect")
    };
    assert!(queued.contains(&meeting), "got {queued:?}");
}

#[test]
fn an_uploaded_meeting_that_already_has_turns_gets_no_job() {
    // Uploads join on the basename exactly as microphone clips do, so one that
    // already has turns is not transcribed again.
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-08T12:00:00Z".parse().expect("t");
    let meeting = mic_row(dir.path(), "meeting-20260907-0905", "20260907T090500");

    let meaning = meaning_plane();
    meaning
        .execute(
            "INSERT INTO sources (id, name, kind)
             VALUES ('meeting-20260907-0905', 'Meeting', 'upload')",
            [],
        )
        .expect("meeting source");
    already_transcribed(
        &meaning,
        "meeting-20260907-0905",
        &meeting,
        "2026-09-07T09:05:00+00:00",
    );

    let ingest = store::open(dir.path()).expect("db");
    recalld::ingest_schema::ensure(&ingest).expect("schema");
    assert_eq!(
        derive_segment_jobs(&ingest, &meaning, now, 100).expect("derive"),
        0
    );
}

#[test]
fn a_source_the_meaning_plane_has_never_heard_of_waits() {
    // Neither an error nor a job: nothing could register the clip's audio, so
    // the job would go barren on every pass and sit at the head of the queue.
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-08T12:00:00Z".parse().expect("t");
    mic_row(dir.path(), "newmic", "20260907T090500");

    let meaning = meaning_plane();
    let ingest = store::open(dir.path()).expect("db");
    recalld::ingest_schema::ensure(&ingest).expect("schema");
    assert_eq!(
        derive_segment_jobs(&ingest, &meaning, now, 100).expect("derive"),
        0
    );
}

#[test]
fn a_lease_picks_the_newest_clip_across_sources_not_the_alphabetical_one() {
    // ⚠ `ORDER BY filename DESC` is newest-first only within one source. Across
    // sources it is alphabetical, and `geb` would starve behind every `usb`
    // clip ever recorded.
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-13T12:00:00Z".parse().expect("t");
    let ingest = store::open(dir.path()).expect("db");
    recalld::ingest_schema::ensure(&ingest).expect("schema");

    // geb is later in time and earlier in the alphabet, so the two orderings
    // disagree.
    for (source, stamp, iso) in [
        ("usb", "20260913T100000", "2026-09-13T10:00:00Z"),
        ("geb", "20260913T110000", "2026-09-13T11:00:00Z"),
    ] {
        let filename = format!("{source}-{stamp}.opus");
        store::insert(
            &ingest,
            &store::Row {
                source: source.into(),
                filename: filename.clone(),
                start_utc: iso.into(),
                bytes: 1,
                sha256: "x".into(),
                received_utc: iso.into(),
                sent_utc: None,
            },
        )
        .expect("segment");
        ingest
            .execute(
                "INSERT INTO jobs (kind, filename, created_utc) VALUES (?1, ?2, ?3)",
                (Kind::TranscribeSegment, &filename, iso),
            )
            .expect("job");
    }

    let job = queue::lease(dir.path(), now, &[Kind::TranscribeSegment])
        .expect("lease")
        .expect("a job");
    assert!(
        job.filename.starts_with("geb-"),
        "the NEWEST clip must be leased, got {}",
        job.filename
    );
}

#[test]
fn a_job_whose_blob_the_ingest_plane_has_forgotten_is_not_leasable() {
    // A job with no `segments` row names a blob nothing can fetch; leasing it
    // could only fail.
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-13T12:00:00Z".parse().expect("t");
    let ingest = store::open(dir.path()).expect("db");
    recalld::ingest_schema::ensure(&ingest).expect("schema");
    ingest
        .execute(
            "INSERT INTO jobs (kind, filename, created_utc)
             VALUES (?1, 'usb-20260913T100000.opus', '2026-09-13T10:00:00Z')",
            [Kind::TranscribeSegment],
        )
        .expect("job");

    assert!(
        queue::lease(dir.path(), now, &[Kind::TranscribeSegment])
            .expect("lease")
            .is_none()
    );
}

#[test]
fn a_clip_transcribed_under_another_extension_gets_no_second_job() {
    // One recording can exist under two extensions across the planes, so "already has turns" is keyed on the stem. Keyed
    // on the whole filename, each such clip costs a full transcription that the
    // write then refuses.
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-11T12:00:00Z".parse().expect("t");
    let _ = mic_row(dir.path(), "usb", "20260910T203720");

    let meaning = meaning_plane();
    // The ingest row is `.opus` (`mic_row`); the turns hang off a `.wav` path.
    meaning
        .execute(
            "INSERT INTO audio_segments (source_id, path, start_utc, end_utc, sample_rate, channels)
             VALUES ('usb', '/data/usb/usb-20260910T203720.wav',
                     '2026-09-10T20:37:20+00:00', '2026-09-10T20:38:20+00:00', 16000, 1)",
            [],
        )
        .expect("audio");
    let id = meaning.last_insert_rowid();
    meaning
        .execute(
            "INSERT INTO transcript_segments (audio_segment_id, start_utc, end_utc, text, asr_model)
             VALUES (?1, '2026-09-10T20:37:20+00:00', '2026-09-10T20:37:25+00:00',
                     'already transcribed', 'whisper')",
            [id],
        )
        .expect("turn");

    let ingest = store::open(dir.path()).expect("db");
    recalld::ingest_schema::ensure(&ingest).expect("schema");
    assert_eq!(
        derive_segment_jobs(&ingest, &meaning, now, 100).expect("derive"),
        0,
        "the container differs; the recording does not"
    );
}

#[test]
fn an_old_outcome_sentence_is_split_into_its_word_and_its_detail() {
    // Before `detail`, counts were written into the outcome as prose.
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = store::open(dir.path()).expect("db");
    conn.execute_batch(
        "INSERT INTO pass_ledger (kind, filename, outcome, decided_utc) VALUES
             ('diarize-segment', 'a.flac', 'attributed: 2 turn(s) named in place', 'then'),
             ('diarize-segment', 'b.flac', 'aligned', 'then');",
    )
    .expect("old rows");
    recalld::ingest_schema::ensure(&conn).expect("reopen");
    let rows: Vec<(String, Option<String>)> = conn
        .prepare("SELECT outcome, detail FROM pass_ledger ORDER BY filename")
        .expect("prep")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    assert_eq!(
        rows,
        vec![
            (
                "attributed".to_owned(),
                Some(r#"{"was":"attributed: 2 turn(s) named in place"}"#.to_owned())
            ),
            ("aligned".to_owned(), None),
        ]
    );
}
