//! The job queue: derive, lease newest first, done; a lapsed lease re-offers.

use audiocore::instant::IngestStamp;
use audiocore::job::Kind;
use chrono::{DateTime, Duration, Utc};
use recalld::queue::{done, lease};
use recalld::store;
use transcript::SourceId;

/// A room block from when the room stream ran in production: history now.
fn room_row(root: &std::path::Path, stamp: &str) {
    let conn = store::open(root).expect("db");
    store::insert(
        &conn,
        &store::Row {
            source: SourceId::parse("room").expect("source id"),
            filename: format!("room-{stamp}.flac"),
            // As production stores it: the name's stamp, parsed.
            start_utc: audiocore::names::parse("room", &format!("room-{stamp}.flac"))
                .expect("a segment name")
                .start_utc,
            bytes: 1,
            sha256: "x".into(),
            received_utc: IngestStamp::parse("2026-09-05T00:00:00Z").expect("instant"),
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
    // A clip that crashes the shim is retired as a failure after the cap.
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
    measured(dir.path(), &silent, 0.0);
    let speech = mic_row(dir.path(), "usb", "20260906T100100");
    let undecodable = mic_row(dir.path(), "usb", "20260906T100200");
    measured(dir.path(), &undecodable, recalld::speech::UNKNOWN_SECONDS);
    let ingest = store::open(dir.path()).expect("db");
    let now: DateTime<Utc> = "2026-09-06T10:30:00Z".parse().expect("t");
    derive_segment_jobs(&ingest, &meaning_plane(), now, 100).expect("derive");
    let mut queued = queued_names(&ingest);
    queued.sort();
    assert_eq!(
        queued,
        vec![speech, undecodable],
        "the SILENT segment must not be queued"
    );
}

#[test]
fn a_segment_waits_for_its_speech_measurement() {
    // A job before the measurement turned silent minutes into "Thank you."
    let dir = tempfile::tempdir().expect("tempdir");
    let clip = unmeasured_row(dir.path(), "usb", "20260906T100000");
    let ingest = store::open(dir.path()).expect("db");
    let now: DateTime<Utc> = "2026-09-06T10:30:00Z".parse().expect("t");
    derive_segment_jobs(&ingest, &meaning_plane(), now, 100).expect("derive");
    assert_eq!(
        queued_names(&ingest),
        Vec::<String>::new(),
        "queued before measured"
    );

    measured(dir.path(), &clip, 12.0);
    derive_segment_jobs(&ingest, &meaning_plane(), now, 100).expect("derive");
    assert_eq!(queued_names(&ingest), vec![clip], "measured speech queues");
}

fn queued_names(ingest: &rusqlite::Connection) -> Vec<String> {
    ingest
        .prepare("SELECT filename FROM jobs")
        .expect("prep")
        .query_map([], |r| r.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
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
    // Derived from a succeeded transcription: spans need words to align.
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
    // A refused clip is not diarized.
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
    // A runner holds one shim's weights.
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

// ---- per-mic transcribe jobs ----

use recalld::queue::{self, derive_segment_jobs};

/// A clip the speech pass has measured as twelve seconds of speech.
fn mic_row(root: &std::path::Path, source: &str, stamp: &str) -> String {
    let name = unmeasured_row(root, source, stamp);
    measured(root, &name, 12.0);
    name
}

fn measured(root: &std::path::Path, name: &str, seconds: f64) {
    let source = name.split('-').next().expect("source");
    store::open(root)
        .expect("db")
        .execute(
            "INSERT OR REPLACE INTO segment_speech (filename, source, speech_seconds, computed_utc)
             VALUES (?1, ?2, ?3, '2026-09-06T10:01:00Z')",
            (name, source, seconds),
        )
        .expect("speech row");
}

/// A clip the speech pass has not reached yet.
fn unmeasured_row(root: &std::path::Path, source: &str, stamp: &str) -> String {
    unmeasured_file(root, source, stamp, "opus")
}

fn unmeasured_file(root: &std::path::Path, source: &str, stamp: &str, ext: &str) -> String {
    let name = format!("{source}-{stamp}.{ext}");
    let conn = store::open(root).expect("db");
    store::insert(
        &conn,
        &store::Row {
            source: SourceId::parse(source).expect("source id"),
            filename: name.clone(),
            // The ingest plane's spelling: a trailing Z.
            start_utc: IngestStamp::parse(&format!(
                "{}-{}-{}T{}:{}:{}Z",
                &stamp[0..4],
                &stamp[4..6],
                &stamp[6..8],
                &stamp[9..11],
                &stamp[11..13],
                &stamp[13..15]
            ))
            .expect("instant"),
            bytes: 1,
            sha256: "x".into(),
            received_utc: IngestStamp::parse("2026-09-05T00:00:00Z").expect("instant"),
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

/// Register a segment as transcribed before the jobs table, in the meaning
/// plane's own spellings.
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
            "INSERT INTO transcribed_before_jobs (audio_segment_id) VALUES (?1)",
            [id],
        )
        .expect("transcribed");
}

#[test]
fn a_segment_that_already_has_turns_gets_no_job() {
    // Joined on the filename: the planes spell an instant `...Z` and
    // `...+00:00`, and as text they never match.
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
    // The room stream's old blocks are history, not microphone clips.
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
    // Bounded, so a backlog does not queue days of GPU work at once.
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
    // Uploads are transcribed through this queue too.
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
    // An upload that has turns is not transcribed again.
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
    // No job: nothing could register this clip's audio.
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
    // By filename, `geb` would wait behind every `usb` clip ever recorded.
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
                source: SourceId::parse(source).expect("source id"),
                filename: filename.clone(),
                start_utc: IngestStamp::parse(iso).expect("instant"),
                bytes: 1,
                sha256: "x".into(),
                received_utc: IngestStamp::parse(iso).expect("instant"),
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
    // A job with no `segments` row names a blob nothing can fetch.
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
    // One recording can have two extensions across the planes, so "already
    // transcribed" is keyed on the stem.
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-11T12:00:00Z".parse().expect("t");
    let _ = mic_row(dir.path(), "usb", "20260910T203720");

    let meaning = meaning_plane();
    // The ingest row is `.opus` (`mic_row`); the transcription names a `.wav` path.
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
            "INSERT INTO transcribed_before_jobs (audio_segment_id) VALUES (?1)",
            [id],
        )
        .expect("transcribed");

    let ingest = store::open(dir.path()).expect("db");
    recalld::ingest_schema::ensure(&ingest).expect("schema");
    assert_eq!(
        derive_segment_jobs(&ingest, &meaning, now, 100).expect("derive"),
        0,
        "the container differs; the recording does not"
    );
}

/// A phone's minute arrives twice, stamped up to a second apart: one
/// transcription per minute.
fn copy_row(root: &std::path::Path, stamp: &str, ext: &str) -> String {
    let name = unmeasured_file(root, "geb", stamp, ext);
    measured(root, &name, 12.0);
    name
}

#[test]
fn a_phones_two_copies_of_one_minute_get_one_job_the_lossless_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-28T14:00:00Z".parse().expect("t");
    copy_row(dir.path(), "20260928T131615", "flac");
    let wav = copy_row(dir.path(), "20260928T131616", "wav");
    let ingest = store::open(dir.path()).expect("db");

    derive_segment_jobs(&ingest, &meaning_plane(), now, 100).expect("derive");

    assert_eq!(queued_names(&ingest), vec![wav]);
}

#[test]
fn a_copy_arriving_after_its_twin_was_queued_gets_no_job() {
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-28T14:00:00Z".parse().expect("t");
    let flac = copy_row(dir.path(), "20260928T131614", "flac");
    let ingest = store::open(dir.path()).expect("db");
    derive_segment_jobs(&ingest, &meaning_plane(), now, 100).expect("derive");
    copy_row(dir.path(), "20260928T131614", "wav");

    derive_segment_jobs(&ingest, &meaning_plane(), now, 100).expect("derive");

    assert_eq!(queued_names(&ingest), vec![flac]);
}

#[test]
fn a_mics_next_minute_is_not_a_copy() {
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-28T14:00:00Z".parse().expect("t");
    copy_row(dir.path(), "20260928T131615", "flac");
    copy_row(dir.path(), "20260928T131715", "flac");
    let ingest = store::open(dir.path()).expect("db");

    let queued = derive_segment_jobs(&ingest, &meaning_plane(), now, 100).expect("derive");

    assert_eq!(queued, 2);
}

#[test]
fn a_finished_transcription_queues_its_diarization_at_once() {
    // No lease has to run first: done derives it.
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-11T12:00:00Z".parse().expect("t");
    let name = queued(dir.path(), "20260911T100000");
    let job = lease(dir.path(), now, ASR).expect("lease").expect("job");

    assert!(done(dir.path(), job.id, TRANSCRIBED, now).expect("done"));

    assert!(
        kinds_queued(dir.path()).contains(&(Kind::DiarizeSegment.as_str().to_owned(), name)),
        "{:?}",
        kinds_queued(dir.path())
    );
}

#[test]
fn catching_up_follow_ons_takes_no_write_lock_while_it_reads() {
    // The read scans every finished transcription: after a reboot, on a cold
    // disk, a write lock held across it refused every lease for minutes.
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-11T12:00:00Z".parse().expect("t");
    let name = mic_row(dir.path(), "usb", "20260911T100000");
    let conn = store::open(dir.path()).expect("db");
    conn.execute(
        "INSERT INTO jobs (kind, filename, state, created_utc, done_utc, result)
         VALUES (?1, ?2, 'done', '2026-09-11T11:00:00Z', '2026-09-11T11:00:00Z', ?3)",
        (Kind::DiarizeSegment, &name, TRANSCRIBED),
    )
    .expect("its diarization is already queued");
    conn.execute(
        "INSERT INTO jobs (kind, filename, state, created_utc, done_utc, result)
         VALUES (?1, ?2, 'done', '2026-09-11T11:00:00Z', '2026-09-11T11:00:00Z', ?3)",
        (Kind::TranscribeSegment, &name, TRANSCRIBED),
    )
    .expect("finished job");
    let writer = store::open(dir.path()).expect("db");
    writer
        .execute_batch("BEGIN IMMEDIATE")
        .expect("hold the write lock");
    conn.busy_timeout(std::time::Duration::from_millis(100))
        .expect("fail fast");

    assert_eq!(
        recalld::queue::derive_follow_ons(&conn, now).expect("a read needs no write lock"),
        0
    );
}

#[test]
fn a_transcription_finished_without_its_diarization_is_caught_up() {
    // Finished before done derived follow-ons, or by hand: the pass catches up,
    // and a refused one still gets none.
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-11T12:00:00Z".parse().expect("t");
    let ok = mic_row(dir.path(), "usb", "20260911T100000");
    let refused = mic_row(dir.path(), "usb", "20260911T100100");
    let conn = store::open(dir.path()).expect("db");
    for (name, result) in [(&ok, TRANSCRIBED), (&refused, REFUSED)] {
        conn.execute(
            "INSERT INTO jobs (kind, filename, state, created_utc, done_utc, result)
             VALUES (?1, ?2, 'done', '2026-09-11T11:00:00Z', '2026-09-11T11:00:00Z', ?3)",
            (Kind::TranscribeSegment, name, result),
        )
        .expect("finished job");
    }

    assert_eq!(
        recalld::queue::derive_follow_ons(&conn, now).expect("derive"),
        1
    );
    assert_eq!(
        recalld::queue::derive_follow_ons(&conn, now).expect("again"),
        0
    );
    assert!(kinds_queued(dir.path()).contains(&(Kind::DiarizeSegment.as_str().to_owned(), ok)));
}

/// The SQLite VM steps one lease takes with `finished` transcriptions stored,
/// each with a 10 kB result.
fn lease_work(finished: usize) -> u64 {
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-11T12:00:00Z".parse().expect("t");
    queued(dir.path(), "20260911T235900");
    let mut conn = store::open(dir.path()).expect("db");
    let body = format!(
        r#"{{"ok":true,"result":{{"text":"{}"}}}}"#,
        "x".repeat(10_000)
    );
    for i in 0..finished {
        let name = mic_row(
            dir.path(),
            "usb",
            &format!("20260910T{:02}{:02}00", i / 60, i % 60),
        );
        conn.execute(
            "INSERT INTO jobs (kind, filename, state, created_utc, done_utc, result)
             VALUES (?1, ?2, 'done', '2026-09-11T11:00:00Z', '2026-09-11T11:00:00Z', ?3)",
            (Kind::TranscribeSegment, &name, &body),
        )
        .expect("finished job");
        conn.execute(
            "INSERT INTO jobs (kind, filename, state, created_utc, done_utc, result)
             VALUES (?1, ?2, 'done', '2026-09-11T11:00:00Z', '2026-09-11T11:00:00Z', '{}')",
            (Kind::DiarizeSegment, &name),
        )
        .expect("its diarization");
    }
    let steps = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let counted = steps.clone();
    conn.progress_handler(
        1,
        Some(move || {
            counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            false
        }),
    )
    .expect("handler");
    recalld::queue::lease_on(&mut conn, now, ASR)
        .expect("lease")
        .expect("job");
    steps.load(std::sync::atomic::Ordering::Relaxed)
}

#[test]
fn a_lease_costs_the_same_whatever_the_archive_holds() {
    // Every lease used to json-parse every finished result under the write
    // lock: 25 s on a cold disk, and the runners' 500s.
    let small = lease_work(10);
    let large = lease_work(1_000);
    assert!(
        large < small * 3,
        "a lease grew with the finished jobs: {small} steps at 10, {large} at 1000"
    );
}

#[test]
fn a_renewed_lease_holds_past_its_first_ten_minutes_and_a_finished_job_is_not_renewed() {
    // Diarizing a long meeting took 11 min: past a flat 10-min lease.
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-11T12:00:00Z".parse().expect("t");
    queued(dir.path(), "20260911T100000");
    let job = lease(dir.path(), now, ASR).expect("lease").expect("job");

    let later = now + Duration::minutes(8);
    assert!(recalld::queue::renew(dir.path(), job.id, later).expect("renew"));
    assert!(
        lease(dir.path(), now + Duration::minutes(12), ASR)
            .expect("lease")
            .is_none(),
        "still held at 12 min"
    );

    assert!(done(dir.path(), job.id, TRANSCRIBED, later).expect("done"));
    assert!(!recalld::queue::renew(dir.path(), job.id, later).expect("renew"));
}
