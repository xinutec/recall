//! E1 lifecycle, lean: derive → newest-first lease → done; a lapsed lease
//! re-offers.

use chrono::{DateTime, Duration, Utc};
use recalld::queue::{DIARIZE_ROOM, TRANSCRIBE_ROOM, done, lease};
use recalld::store;

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

#[test]
fn newest_first_lease_done_and_lapse() {
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-05T12:00:00Z".parse().expect("t");
    room_row(dir.path(), "20260905T100000");
    room_row(dir.path(), "20260905T110000");
    // Newest first.
    let first = lease(dir.path(), now, &[TRANSCRIBE_ROOM])
        .expect("lease")
        .expect("job");
    assert_eq!(first.filename, "room-20260905T110000.flac");
    // The leased job is not re-offered while its lease holds…
    let second = lease(dir.path(), now, &[TRANSCRIBE_ROOM])
        .expect("lease")
        .expect("job");
    assert_eq!(second.filename, "room-20260905T100000.flac");
    assert!(
        lease(dir.path(), now, &[TRANSCRIBE_ROOM])
            .expect("lease")
            .is_none()
    );
    // …but a lapsed lease re-offers, and done retires for good.
    let later = now + Duration::minutes(20);
    let again = lease(dir.path(), later, &[TRANSCRIBE_ROOM])
        .expect("lease")
        .expect("job");
    assert_eq!(again.filename, "room-20260905T110000.flac");
    assert!(done(dir.path(), again.id, "{}", later).expect("done"));
    assert!(!done(dir.path(), again.id, "{}", later).expect("idempotent"));
    let last = lease(
        dir.path(),
        later + Duration::minutes(20),
        &[TRANSCRIBE_ROOM],
    )
    .expect("lease")
    .expect("job");
    assert_eq!(last.filename, "room-20260905T100000.flac");
}

#[test]
fn a_job_nobody_finishes_is_retired_after_its_attempts_are_spent() {
    // A clip that crashes the shim never reaches `done`: its lease lapses and
    // it is offered again, newest first, so without a cap it holds the runner
    // for ever. After the cap it is a recorded failure like a refusal.
    let dir = tempfile::tempdir().expect("tempdir");
    let start: DateTime<Utc> = "2026-09-05T12:00:00Z".parse().expect("t");
    room_row(dir.path(), "20260905T100000");
    let mut now = start;
    for attempt in 1..=queue::MAX_ATTEMPTS {
        let job = lease(dir.path(), now, &[TRANSCRIBE_ROOM])
            .expect("lease")
            .unwrap_or_else(|| panic!("attempt {attempt} must still be offered"));
        assert_eq!(job.filename, "room-20260905T100000.flac");
        now += Duration::minutes(20);
    }
    assert!(
        lease(dir.path(), now, &[TRANSCRIBE_ROOM])
            .expect("lease")
            .is_none(),
        "spent: not offered again"
    );
    let (state, result): (String, String) = store::open(dir.path())
        .expect("db")
        .query_row(
            "SELECT state, result FROM jobs WHERE filename = 'room-20260905T100000.flac'",
            [],
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
    // Transcribing silence does not return nothing — it returns INVENTIONS.
    // Measured on the live queue 2026-09-06: a silent minute came back as
    // "Thank you." twice, another as 156 segments with a 150-character run of
    // tildes at 0.19 confidence (#1410). 42% of the queue was silence.
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = store::open(dir.path()).expect("db");
    recalld::queue::ensure_schema(&conn).expect("jobs schema");
    recalld::speech::ensure_schema(&conn).expect("speech schema");
    for (name, seconds) in [
        ("room-20260906T100000.flac", Some(0.0)),
        ("room-20260906T100100.flac", Some(12.0)),
        ("room-20260906T100200.flac", None),
    ] {
        store::insert(
            &conn,
            &store::Row {
                source: "room".to_owned(),
                filename: name.to_owned(),
                start_utc: "2026-09-06T10:00:00Z".to_owned(),
                bytes: 1,
                sha256: "x".to_owned(),
                received_utc: "2026-09-06T10:00:30Z".to_owned(),
                sent_utc: None,
            },
        )
        .expect("row");
        if let Some(seconds) = seconds {
            conn.execute(
                "INSERT INTO segment_speech (filename, source, speech_seconds, computed_utc)
                 VALUES (?1, 'room', ?2, '2026-09-06T10:01:00Z')",
                (name, seconds),
            )
            .expect("speech row");
        }
    }
    let now = chrono::DateTime::parse_from_rfc3339("2026-09-06T10:30:00Z")
        .expect("t")
        .with_timezone(&chrono::Utc);
    recalld::queue::derive_jobs(&conn, now).expect("derive");
    let mut queued: Vec<String> = conn
        .prepare("SELECT filename FROM jobs ORDER BY filename")
        .expect("prep")
        .query_map([], |r| r.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    queued.sort();
    assert_eq!(
        queued,
        vec![
            // speech: queued.
            "room-20260906T100100.flac".to_owned(),
            // unmeasured: queued too — "not looked at yet" is not evidence of
            // silence, and a host without a detector must still do work.
            "room-20260906T100200.flac".to_owned(),
        ],
        "the SILENT segment must not be queued"
    );
}

/// The transcription result shape the ASR shim actually returns, trimmed to what
/// the derivation reads. Written out rather than `{"ok": true}` so the test fails
/// if the real envelope ever stops being the thing being checked.
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
    // Diarization alone attributes nothing: it yields SPEAKER_00 spans, and it is
    // the alignment against words that makes them turns. So the job is derived
    // from a SUCCEEDED transcription, never from the segment.
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-11T12:00:00Z".parse().expect("t");
    room_row(dir.path(), "20260911T100000");

    let job = lease(dir.path(), now, &[TRANSCRIBE_ROOM])
        .expect("lease")
        .expect("job");
    assert_eq!(job.kind, TRANSCRIBE_ROOM);
    assert!(
        !kinds_queued(dir.path())
            .iter()
            .any(|(kind, _)| kind == DIARIZE_ROOM),
        "no diarize job before the block is transcribed"
    );

    assert!(done(dir.path(), job.id, TRANSCRIBED, now).expect("done"));
    let next = lease(dir.path(), now, &[DIARIZE_ROOM])
        .expect("lease")
        .expect("job");
    assert_eq!(next.kind, DIARIZE_ROOM);
    assert_eq!(next.filename, "room-20260911T100000.flac");
}

#[test]
fn a_refused_transcription_derives_no_diarization() {
    // A block the ASR refused is a block whose CLIP is the problem
    // (`turns::Barren::Refused`). Handing the same clip to pyannote spends
    // GPU to learn that again.
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-11T12:00:00Z".parse().expect("t");
    room_row(dir.path(), "20260911T100000");
    let job = lease(dir.path(), now, &[TRANSCRIBE_ROOM])
        .expect("lease")
        .expect("job");
    assert!(done(dir.path(), job.id, REFUSED, now).expect("done"));

    assert!(
        lease(dir.path(), now, &[DIARIZE_ROOM])
            .expect("lease")
            .is_none(),
        "a refused clip must not be queued for diarization"
    );
}

#[test]
fn a_runner_is_never_handed_a_kind_it_cannot_do() {
    // A runner holds ONE shim's weights. Offering it another kind would burn the
    // job's attempts against a process that can never do it.
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-11T12:00:00Z".parse().expect("t");
    room_row(dir.path(), "20260911T100000");
    let job = lease(dir.path(), now, &[TRANSCRIBE_ROOM])
        .expect("lease")
        .expect("job");
    assert!(done(dir.path(), job.id, TRANSCRIBED, now).expect("done"));

    // Only a diarize job is now outstanding, and an asr-only runner sees nothing.
    assert!(
        lease(dir.path(), now, &[TRANSCRIBE_ROOM])
            .expect("lease")
            .is_none()
    );
    // "I can do nothing" leases nothing, rather than everything.
    assert!(lease(dir.path(), now, &[]).expect("lease").is_none());
    // A runner that can do both takes it.
    let both = lease(dir.path(), now, &[TRANSCRIBE_ROOM, DIARIZE_ROOM])
        .expect("lease")
        .expect("job");
    assert_eq!(both.kind, DIARIZE_ROOM);
}

// ---- per-mic transcribe jobs: the orchestration port (#1538) ----

use recalld::queue::{self, TRANSCRIBE_SEGMENT, derive_segment_jobs, ensure_schema};

fn mic_row(root: &std::path::Path, source: &str, stamp: &str) -> String {
    let name = format!("{source}-{stamp}.opus");
    let conn = store::open(root).expect("db");
    store::insert(
        &conn,
        &store::Row {
            source: source.into(),
            filename: name.clone(),
            // ⚠ The INGEST plane's spelling: a trailing Z.
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

/// A meaning plane with the shape `derive_segment_jobs` reads.
fn meaning_plane() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().expect("mem");
    conn.execute_batch(
        "CREATE TABLE sources (id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL);
         CREATE TABLE audio_segments (
             id INTEGER PRIMARY KEY, source_id TEXT NOT NULL, path TEXT NOT NULL,
             start_utc TEXT NOT NULL);
         CREATE TABLE transcript_segments (
             id INTEGER PRIMARY KEY, audio_segment_id INTEGER, text TEXT NOT NULL);
         INSERT INTO sources (id, name, kind) VALUES
             ('usb', 'usb', 'coreaudio'),
             ('geb', 'geb', 'tcp_pcm'),
             ('room', 'Room', 'derived');",
    )
    .expect("schema");
    conn
}

/// Register a segment as transcribed, in the MEANING plane's own spellings.
fn already_transcribed(meaning: &rusqlite::Connection, source: &str, filename: &str, start: &str) {
    meaning
        .execute(
            "INSERT INTO audio_segments (source_id, path, start_utc) VALUES (?1, ?2, ?3)",
            (source, format!("/data/{source}/{filename}"), start),
        )
        .expect("audio");
    let id = meaning.last_insert_rowid();
    meaning
        .execute(
            "INSERT INTO transcript_segments (audio_segment_id, text) VALUES (?1, 'some words')",
            [id],
        )
        .expect("turn");
}

#[test]
fn a_segment_that_already_has_turns_gets_no_job() {
    // ⚠ THE JOIN KEY IS THE FILENAME, AND THAT IS NOT A STYLE CHOICE. The obvious
    // join — start_utc to start_utc — matches NOTHING: this ingest row says
    // `2026-09-05T10:00:00Z` and the meaning row `2026-09-05T10:00:00+00:00`.
    // Same instant, different spelling, compared as text. It returns a confident
    // zero rather than an error, which is how it cost a real measurement.
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-05T12:00:00Z".parse().expect("t");
    let done_one = mic_row(dir.path(), "usb", "20260905T100000");
    let todo = mic_row(dir.path(), "usb", "20260905T110000");

    let meaning = meaning_plane();
    already_transcribed(&meaning, "usb", &done_one, "2026-09-05T10:00:00+00:00");

    let ingest = store::open(dir.path()).expect("db");
    ensure_schema(&ingest).expect("schema");
    assert_eq!(
        derive_segment_jobs(&ingest, &meaning, now, 100).expect("derive"),
        1,
        "exactly the untranscribed one"
    );
    let queued: String = ingest
        .query_row(
            "SELECT filename FROM jobs WHERE kind = ?1",
            [TRANSCRIBE_SEGMENT],
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
    // The room stream has its own kind; deriving both for one blob would
    // transcribe it twice and pay the GPU bill twice.
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-05T12:00:00Z".parse().expect("t");
    room_row(dir.path(), "20260905T100000");
    let meaning = meaning_plane();
    let ingest = store::open(dir.path()).expect("db");
    ensure_schema(&ingest).expect("schema");
    assert_eq!(
        derive_segment_jobs(&ingest, &meaning, now, 100).expect("derive"),
        0
    );
}

#[test]
fn the_derivation_is_bounded_and_newest_first() {
    // ⚠ 14,078 segments were untranscribed when this was written. Deriving them
    // all in one statement would queue days of GPU work at once, competing with
    // the room stream and with capture. The bound is what makes it reversible.
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-05T12:00:00Z".parse().expect("t");
    for minute in 0..5 {
        mic_row(dir.path(), "usb", &format!("20260905T10{minute:02}00"));
    }
    let meaning = meaning_plane();
    let ingest = store::open(dir.path()).expect("db");
    ensure_schema(&ingest).expect("schema");
    assert_eq!(
        derive_segment_jobs(&ingest, &meaning, now, 2).expect("derive"),
        2,
        "the limit bounds the work queued"
    );
    let newest: String = ingest
        .query_row(
            "SELECT filename FROM jobs WHERE kind = ?1 ORDER BY filename DESC LIMIT 1",
            [TRANSCRIBE_SEGMENT],
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
    ensure_schema(&ingest).expect("schema");
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
    // ⚠ This test asserted the OPPOSITE until 2026-09-17, on the belief that
    // "recalld transcribes them on arrival". It does not and never did: an upload
    // reached the Mac through `/sync/jobs`, whose consumer (`recall jobs`) was
    // deleted with refine. Excluding uploads here left the feature with no
    // transcriber at all (#1649). The ingest plane is the one road now.
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
    ensure_schema(&ingest).expect("schema");
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
            .query_map([TRANSCRIBE_SEGMENT], |r| r.get::<_, String>(0))
            .expect("rows");
        rows.collect::<Result<_, _>>().expect("collect")
    };
    assert!(queued.contains(&meeting), "got {queued:?}");
}

#[test]
fn an_uploaded_meeting_that_already_has_turns_gets_no_job() {
    // The guard that makes the line above safe: 22 uploads were already
    // transcribed when uploads were admitted, and re-deriving them would have
    // spent the GPU on turns that exist. Their basenames are in the `have` set
    // exactly as a microphone clip's are — the road differed, the join key never
    // did.
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
    ensure_schema(&ingest).expect("schema");
    assert_eq!(
        derive_segment_jobs(&ingest, &meaning, now, 100).expect("derive"),
        0
    );
}

#[test]
fn a_source_the_meaning_plane_has_never_heard_of_waits() {
    // Not an error, and not a job either. Nothing could register that clip's
    // audio, so a job for it could only go barren on every pass — and a barren
    // clip with no ledger row sits at the head of the queue for ever.
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-08T12:00:00Z".parse().expect("t");
    mic_row(dir.path(), "newmic", "20260907T090500");

    let meaning = meaning_plane();
    let ingest = store::open(dir.path()).expect("db");
    ensure_schema(&ingest).expect("schema");
    assert_eq!(
        derive_segment_jobs(&ingest, &meaning, now, 100).expect("derive"),
        0
    );
}

#[test]
fn a_lease_picks_the_newest_clip_across_sources_not_the_alphabetical_one() {
    // ⚠ THE STARVATION BUG THIS ORDERING EXISTS TO AVOID. `ORDER BY filename
    // DESC` is newest-first only while every job is `room-*`. With microphone
    // clips in the queue it becomes source-alphabetical — `usb-` above `room-`
    // above `geb-` — so a runner would transcribe every usb clip ever recorded
    // before geb got one job. Nothing would look like an ordering fault; geb
    // would simply have no transcripts.
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-13T12:00:00Z".parse().expect("t");
    let ingest = store::open(dir.path()).expect("db");
    ensure_schema(&ingest).expect("schema");

    // geb is LATER in time and EARLIER in the alphabet — the two orderings
    // disagree, which is the only case that can tell them apart.
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
                (TRANSCRIBE_SEGMENT, &filename, iso),
            )
            .expect("job");
    }

    let job = queue::lease(dir.path(), now, &[TRANSCRIBE_SEGMENT])
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
    // The join's other edge. A job with no `segments` row names a blob nothing
    // can fetch, so offering it would spend a runner's lease on work it can
    // only fail — and the attempts would cycle.
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-13T12:00:00Z".parse().expect("t");
    let ingest = store::open(dir.path()).expect("db");
    ensure_schema(&ingest).expect("schema");
    ingest
        .execute(
            "INSERT INTO jobs (kind, filename, created_utc)
             VALUES (?1, 'usb-20260913T100000.opus', '2026-09-13T10:00:00Z')",
            [TRANSCRIBE_SEGMENT],
        )
        .expect("job");

    assert!(
        queue::lease(dir.path(), now, &[TRANSCRIBE_SEGMENT])
            .expect("lease")
            .is_none()
    );
}

#[test]
fn a_clip_transcribed_under_another_extension_gets_no_second_job() {
    // ⚠ MEASURED ON THE LIVE FLEET, 2026-09-13. The same recording exists as
    // `.wav` in the ingest plane and `.opus` in the mirror the meaning plane's
    // path points at — 22,313 clips under 20,728 stems. Keyed on the whole
    // filename, 244 of 950 queued jobs were for clips that ALREADY HAD TURNS:
    // a full transcription each, ~50 s of GPU, refused at the write.
    //
    // Nothing was corrupted — that refusal is the design — but the queue drains
    // slower and the GPU runs hot for transcripts thrown away, and the only
    // symptom is the absence of progress.
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-11T12:00:00Z".parse().expect("t");
    let _ = mic_row(dir.path(), "usb", "20260910T203720");

    let meaning = meaning_plane();
    // The turns hang off the OPUS row; the ingest copy this test derives from
    // is the `.opus`-vs-`.wav` pair's other half.
    meaning
        .execute(
            "INSERT INTO audio_segments (source_id, path, start_utc)
             VALUES ('usb', '/data/usb/usb-20260910T203720.opus',
                     '2026-09-10T20:37:20+00:00')",
            [],
        )
        .expect("audio");
    let id = meaning.last_insert_rowid();
    meaning
        .execute(
            "INSERT INTO transcript_segments (audio_segment_id, text)
             VALUES (?1, 'already transcribed')",
            [id],
        )
        .expect("turn");

    let ingest = store::open(dir.path()).expect("db");
    ensure_schema(&ingest).expect("schema");
    assert_eq!(
        derive_segment_jobs(&ingest, &meaning, now, 100).expect("derive"),
        0,
        "the container differs; the recording does not"
    );
}
