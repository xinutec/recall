//! Uploaded meetings: the list, the guards, and the transcript export.
//!
//! ⚠ Rename and re-diarize reach the sources table, so the guards keep the
//! household capture archive unreachable through a path meant for meetings.

use recalld::sessions::{
    self, ExportTurn, SessionError, clean_transcript, name_voice, rediarize, rename,
};
use rusqlite::Connection;

const NOW: &str = "2026-09-07T09:00:00+00:00";

fn schema(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE sources (id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL);
         CREATE TABLE audio_segments (
            id INTEGER PRIMARY KEY AUTOINCREMENT, source_id TEXT NOT NULL,
            path TEXT, start_utc TEXT NOT NULL, end_utc TEXT NOT NULL);
         CREATE TABLE transcript_segments (
            id INTEGER PRIMARY KEY AUTOINCREMENT, audio_segment_id INTEGER,
            start_utc TEXT NOT NULL, end_utc TEXT NOT NULL, text TEXT NOT NULL,
            speaker_label TEXT, speaker_cluster TEXT,
            superseded_by INTEGER, hidden_reason TEXT);",
    )
    .expect("schema");
}

fn source(conn: &Connection, id: &str, kind: &str) {
    conn.execute(
        "INSERT INTO sources (id, name, kind) VALUES (?1, ?1, ?2)",
        (id, kind),
    )
    .expect("source");
}

fn segment(conn: &Connection, source: &str, start: &str, end: &str) -> i64 {
    conn.execute(
        "INSERT INTO audio_segments (source_id, path, start_utc, end_utc) VALUES (?1, 'x', ?2, ?3)",
        (source, start, end),
    )
    .expect("segment");
    conn.last_insert_rowid()
}

fn turn(
    conn: &Connection,
    seg: i64,
    start: &str,
    text: &str,
    label: Option<&str>,
    cluster: Option<&str>,
) {
    conn.execute(
        "INSERT INTO transcript_segments
             (audio_segment_id, start_utc, end_utc, text, speaker_label, speaker_cluster)
         VALUES (?1, ?2, ?2, ?3, ?4, ?5)",
        (seg, start, text, label, cluster),
    )
    .expect("turn");
}

fn db() -> Connection {
    let conn = Connection::open_in_memory().expect("open");
    schema(&conn);
    conn
}

#[test]
fn only_uploaded_sessions_are_listed_never_the_household_capture() {
    // ⚠ The continuous archive is not a "session" and must never appear in a
    // list whose every other action is rename/delete/re-diarize.
    let conn = db();
    source(&conn, "meeting-1", "upload");
    source(&conn, "usb", "coreaudio");
    let m = segment(
        &conn,
        "meeting-1",
        "2026-07-03T09:50:00+00:00",
        "2026-07-03T10:20:00+00:00",
    );
    let u = segment(
        &conn,
        "usb",
        "2026-07-03T09:00:00+00:00",
        "2026-07-03T11:00:00+00:00",
    );
    turn(
        &conn,
        m,
        "2026-07-03T09:51:00+00:00",
        "hello",
        Some("Dr Smith"),
        None,
    );
    turn(
        &conn,
        u,
        "2026-07-03T09:05:00+00:00",
        "kitchen noise",
        None,
        None,
    );

    let out = sessions::sessions(&conn).expect("list");

    assert_eq!(out.items.len(), 1);
    assert_eq!(out.items[0].id, "meeting-1");
    assert_eq!(out.items[0].turn_count, 1);
}

#[test]
fn a_diarization_tag_is_never_listed_as_a_person() {
    // A diarization cluster tag is about voices, not people; listing it would put
    // a machine label where a name goes.
    let conn = db();
    source(&conn, "meeting-1", "upload");
    let m = segment(
        &conn,
        "meeting-1",
        "2026-07-03T09:50:00+00:00",
        "2026-07-03T10:20:00+00:00",
    );
    turn(
        &conn,
        m,
        "2026-07-03T09:51:00+00:00",
        "a",
        Some("SPEAKER_00"),
        None,
    );
    turn(
        &conn,
        m,
        "2026-07-03T09:52:00+00:00",
        "b",
        Some("Dr Smith"),
        None,
    );

    let out = sessions::sessions(&conn).expect("list");

    assert_eq!(out.items[0].speakers, vec!["Dr Smith", "unknown"]);
}

#[test]
fn a_session_with_no_turns_still_lists_with_an_empty_speaker_set() {
    // A freshly uploaded meeting appears at once, at 0 turns, while the worker
    // transcribes it; otherwise an upload would look like it failed.
    let conn = db();
    source(&conn, "meeting-1", "upload");
    segment(
        &conn,
        "meeting-1",
        "2026-07-03T09:50:00+00:00",
        "2026-07-03T10:20:00+00:00",
    );

    let out = sessions::sessions(&conn).expect("list");

    assert_eq!(out.items[0].turn_count, 0);
    assert!(out.items[0].speakers.is_empty());
}

#[test]
fn renaming_a_session_changes_its_displayed_title() {
    let conn = db();
    source(&conn, "meeting-1", "upload");

    rename(&conn, "meeting-1", "Neuro-oncology clinic").expect("renamed");

    let name: String = conn
        .query_row("SELECT name FROM sources WHERE id = 'meeting-1'", [], |r| {
            r.get(0)
        })
        .expect("read back");
    assert_eq!(name, "Neuro-oncology clinic");
}

#[test]
fn renaming_the_household_archive_is_refused() {
    let conn = db();
    source(&conn, "usb", "coreaudio");

    let err = rename(&conn, "usb", "nice try").expect_err("must refuse");

    assert!(matches!(err, SessionError::NotAnUpload), "got {err:?}");
    let name: String = conn
        .query_row("SELECT name FROM sources WHERE id = 'usb'", [], |r| {
            r.get(0)
        })
        .expect("read back");
    assert_eq!(name, "usb", "the archive's name is untouched");
}

#[test]
fn renaming_a_session_that_does_not_exist_is_a_miss_not_a_refusal() {
    let conn = db();

    let err = rename(&conn, "meeting-nope", "x").expect_err("must fail");

    assert!(matches!(err, SessionError::Missing), "got {err:?}");
}

/// A meeting whose clips are delivered, transcribed and diarized: the shape
/// `rediarize` acts on. Returns the data root.
fn diarized_meeting(source: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let meaning = recalld::work::open_write(root).expect("meaning");
    recalld::meaning_schema::ensure(&meaning).expect("schema");
    meaning
        .execute(
            "INSERT INTO sources (id, name, kind) VALUES (?1, ?1, 'upload')",
            [source],
        )
        .expect("source");
    let ingest = recalld::store::open(root).expect("ingest");
    recalld::ingest_schema::ensure(&ingest).expect("jobs");
    for stamp in ["20260703T095000", "20260703T100000"] {
        let filename = format!("{source}-{stamp}.mp3");
        recalld::store::insert(
            &ingest,
            &recalld::store::Row {
                source: source.to_owned(),
                filename: filename.clone(),
                start_utc: "2026-07-03T09:50:00Z".to_owned(),
                bytes: 1,
                sha256: "x".to_owned(),
                received_utc: NOW.to_owned(),
                sent_utc: None,
            },
        )
        .expect("blob");
        for kind in ["transcribe-segment", "diarize-segment"] {
            ingest
                .execute(
                    "INSERT INTO jobs (kind, filename, state, created_utc, done_utc, result)
                     VALUES (?1, ?2, 'done', ?3, ?3, '{\"ok\":true}')",
                    (kind, &filename, NOW),
                )
                .expect("job");
        }
        recalld::turns::ledger(&ingest, "diarize-segment", &filename, "aligned", NOW)
            .expect("ledger row");
    }
    dir
}

fn open_jobs(root: &std::path::Path, kind: &str) -> i64 {
    recalld::store::open(root)
        .expect("ingest")
        .query_row(
            "SELECT count(*) FROM jobs WHERE kind = ?1 AND state = 'queued' AND done_utc IS NULL",
            [kind],
            |r| r.get(0),
        )
        .expect("count")
}

fn ledgered(root: &std::path::Path, kind: &str) -> i64 {
    recalld::store::open(root)
        .expect("ingest")
        .query_row(
            "SELECT count(*) FROM pass_ledger WHERE kind = ?1",
            [kind],
            |r| r.get(0),
        )
        .expect("count")
}

#[test]
fn rediarizing_requeues_every_clip_of_the_meeting_and_nothing_else() {
    let dir = diarized_meeting("meeting-1");
    let root = dir.path();
    let meaning = recalld::work::open_write(root).expect("meaning");
    let ingest = recalld::store::open(root).expect("ingest");

    let requeued = rediarize(&meaning, &ingest, "meeting-1").expect("requeued");

    assert_eq!(requeued, 2);
    assert_eq!(
        open_jobs(root, "diarize-segment"),
        2,
        "both clips are leasable again"
    );
    assert_eq!(
        ledgered(root, "diarize-segment"),
        0,
        "the pass will decide them afresh"
    );
    // The words stand: only who said them is re-derived.
    assert_eq!(open_jobs(root, "transcribe-segment"), 0);
}

#[test]
fn rediarizing_a_session_with_no_audio_says_so() {
    let dir = tempfile::tempdir().expect("tempdir");
    let meaning = recalld::work::open_write(dir.path()).expect("meaning");
    recalld::meaning_schema::ensure(&meaning).expect("schema");
    source(&meaning, "meeting-1", "upload");
    let ingest = recalld::store::open(dir.path()).expect("ingest");

    let err = rediarize(&meaning, &ingest, "meeting-1").expect_err("nothing to redo");

    assert!(matches!(err, SessionError::NoAudio), "got {err:?}");
}

#[test]
fn rediarizing_the_household_archive_is_refused() {
    let dir = diarized_meeting("meeting-1");
    let root = dir.path();
    let meaning = recalld::work::open_write(root).expect("meaning");
    source(&meaning, "usb", "coreaudio");
    let ingest = recalld::store::open(root).expect("ingest");

    let err = rediarize(&meaning, &ingest, "usb").expect_err("must refuse");

    assert!(matches!(err, SessionError::NotAnUpload), "got {err:?}");
    assert_eq!(
        open_jobs(root, "diarize-segment"),
        0,
        "a refused guard requeues nothing"
    );
}

#[test]
fn naming_a_voice_labels_every_turn_of_that_cluster_including_hidden_ones() {
    // ⚠ No hidden_reason filter, unlike the reads. Hiding is a display state;
    // who spoke is a fact. A hidden turn unhidden later must come back named.
    let conn = db();
    source(&conn, "meeting-1", "upload");
    let m = segment(
        &conn,
        "meeting-1",
        "2026-07-03T09:50:00+00:00",
        "2026-07-03T10:20:00+00:00",
    );
    turn(
        &conn,
        m,
        "2026-07-03T09:51:00+00:00",
        "a",
        None,
        Some("SPEAKER_00"),
    );
    turn(
        &conn,
        m,
        "2026-07-03T09:52:00+00:00",
        "b",
        None,
        Some("SPEAKER_01"),
    );
    conn.execute(
        "INSERT INTO transcript_segments
             (audio_segment_id, start_utc, end_utc, text, speaker_cluster, hidden_reason)
         VALUES (?1, '2026-07-03T09:53:00+00:00', '2026-07-03T09:53:00+00:00', 'c',
                 'SPEAKER_00', 'silence')",
        [m],
    )
    .expect("hidden turn");

    let updated = name_voice(&conn, "meeting-1", "SPEAKER_00", Some("Dr Smith")).expect("named");

    assert_eq!(updated, 2, "both SPEAKER_00 turns, hidden included");
    let other: Option<String> = conn
        .query_row(
            "SELECT speaker_label FROM transcript_segments WHERE speaker_cluster = 'SPEAKER_01'",
            [],
            |r| r.get(0),
        )
        .expect("read back");
    assert_eq!(other, None, "the other voice is untouched");
}

#[test]
fn a_superseded_turn_is_not_renamed_by_a_voice_naming() {
    // Its current version carries the human text; renaming the dead one would
    // put a name on a row nobody reads and leave the live one unnamed.
    let conn = db();
    source(&conn, "meeting-1", "upload");
    let m = segment(
        &conn,
        "meeting-1",
        "2026-07-03T09:50:00+00:00",
        "2026-07-03T10:20:00+00:00",
    );
    conn.execute(
        "INSERT INTO transcript_segments
             (audio_segment_id, start_utc, end_utc, text, speaker_cluster, superseded_by)
         VALUES (?1, '2026-07-03T09:51:00+00:00', '2026-07-03T09:51:00+00:00', 'old',
                 'SPEAKER_00', 99)",
        [m],
    )
    .expect("superseded turn");

    let updated = name_voice(&conn, "meeting-1", "SPEAKER_00", Some("Dr Smith")).expect("named");

    assert_eq!(updated, 0);
}

#[test]
fn clearing_a_voice_name_removes_the_label_rather_than_storing_a_blank() {
    let conn = db();
    source(&conn, "meeting-1", "upload");
    let m = segment(
        &conn,
        "meeting-1",
        "2026-07-03T09:50:00+00:00",
        "2026-07-03T10:20:00+00:00",
    );
    turn(
        &conn,
        m,
        "2026-07-03T09:51:00+00:00",
        "a",
        Some("Dr Smith"),
        Some("SPEAKER_00"),
    );

    name_voice(&conn, "meeting-1", "SPEAKER_00", None).expect("cleared");

    let label: Option<String> = conn
        .query_row("SELECT speaker_label FROM transcript_segments", [], |r| {
            r.get(0)
        })
        .expect("read back");
    assert_eq!(label, None);
}

fn export(start: &str, text: &str, label: Option<&str>, cluster: Option<&str>) -> ExportTurn {
    ExportTurn {
        start_utc: start.to_owned(),
        text: text.to_owned(),
        speaker_label: label.map(str::to_owned),
        speaker_cluster: cluster.map(str::to_owned),
    }
}

#[test]
fn consecutive_turns_by_one_speaker_merge_into_a_single_bubble() {
    let turns = vec![
        export(
            "2026-07-03T09:51:00+00:00",
            "hello there",
            Some("Dr Smith"),
            None,
        ),
        export(
            "2026-07-03T09:51:04+00:00",
            "how are you",
            Some("Dr Smith"),
            None,
        ),
        export(
            "2026-07-03T09:51:09+00:00",
            "fine thanks",
            Some("Pippijn"),
            None,
        ),
    ];

    let out = clean_transcript("meeting-1", &turns);

    assert_eq!(out.turns.len(), 2);
    assert_eq!(out.turns[0].text, "hello there how are you");
    assert_eq!(out.turns[0].speaker, "Dr Smith");
    assert_eq!(out.turns[1].text, "fine thanks");
    assert_eq!(out.speakers, vec!["Dr Smith", "Pippijn"]);
}

#[test]
fn an_unnamed_voice_keeps_its_cluster_so_two_strangers_stay_apart() {
    // Falling back to "unknown" for both would merge two people into one bubble.
    let turns = vec![
        export(
            "2026-07-03T09:51:00+00:00",
            "first voice",
            None,
            Some("SPEAKER_00"),
        ),
        export(
            "2026-07-03T09:51:05+00:00",
            "second voice",
            None,
            Some("SPEAKER_01"),
        ),
    ];

    let out = clean_transcript("meeting-1", &turns);

    assert_eq!(out.turns.len(), 2, "two voices, two bubbles");
    assert_eq!(
        out.speakers,
        Vec::<String>::new(),
        "a cluster is not a person"
    );
}

#[test]
fn a_turn_with_neither_name_nor_cluster_reads_as_unknown() {
    let turns = vec![export(
        "2026-07-03T09:51:00+00:00",
        "who said this",
        None,
        None,
    )];

    let out = clean_transcript("meeting-1", &turns);

    assert_eq!(out.turns[0].speaker, "unknown");
}

#[test]
fn an_empty_session_exports_a_null_date_rather_than_failing() {
    let out = clean_transcript("meeting-1", &[]);

    assert_eq!(out.date, None);
    assert!(out.turns.is_empty());
    assert_eq!(out.session, "meeting-1");
}

#[test]
fn the_export_date_is_the_first_bubble_start() {
    let turns = vec![
        export("2026-07-03T09:51:00+00:00", "a", Some("Dr Smith"), None),
        export("2026-07-03T10:20:00+00:00", "b", Some("Dr Smith"), None),
    ];

    let out = clean_transcript("meeting-1", &turns);

    assert_eq!(out.date.as_deref(), Some(out.turns[0].start.as_str()));
}

// --- deleting ----------------------------------------------------------------

use recalld::sessions::delete_session;

/// The full cascade's tables, copied from the production schema: a delete that
/// missed a table would pass against a schema that lacks it.
fn delete_db() -> Connection {
    let conn = Connection::open_in_memory().expect("open");
    conn.execute_batch(
        "CREATE TABLE sources (id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL);
         CREATE TABLE audio_segments (
            id INTEGER PRIMARY KEY AUTOINCREMENT, source_id TEXT NOT NULL,
            path TEXT NOT NULL, start_utc TEXT NOT NULL, end_utc TEXT NOT NULL,
            UNIQUE (source_id, start_utc));
         CREATE TABLE transcript_segments (
            id INTEGER PRIMARY KEY AUTOINCREMENT, audio_segment_id INTEGER,
            start_utc TEXT NOT NULL, end_utc TEXT NOT NULL, text TEXT NOT NULL,
            speaker_label TEXT, speaker_cluster TEXT, superseded_by INTEGER,
            hidden_reason TEXT);
         CREATE TABLE transcript_embeddings (segment_id INTEGER, vec BLOB);
         CREATE TABLE transcript_lineage (derived_id INTEGER, source_id INTEGER);
         CREATE TABLE corrections (
            id INTEGER PRIMARY KEY AUTOINCREMENT, audio_segment_id INTEGER,
            transcript_segment_id INTEGER, corrected_text TEXT);
         CREATE TABLE refine_requests (
            id INTEGER PRIMARY KEY AUTOINCREMENT, source_id TEXT NOT NULL,
            start_utc TEXT NOT NULL, end_utc TEXT NOT NULL, created_utc TEXT NOT NULL);
         CREATE TABLE deleted_segments (
            source_id TEXT NOT NULL, start_utc TEXT NOT NULL, deleted_utc TEXT NOT NULL,
            UNIQUE (source_id, start_utc));",
    )
    .expect("schema");
    conn
}

fn populate(conn: &Connection, source: &str, kind: &str) -> i64 {
    conn.execute(
        "INSERT INTO sources (id, name, kind) VALUES (?1, ?1, ?2)",
        (source, kind),
    )
    .expect("source");
    conn.execute(
        "INSERT INTO audio_segments (source_id, path, start_utc, end_utc)
         VALUES (?1, ?2, '2026-07-03T09:50:00+00:00', '2026-07-03T10:20:00+00:00')",
        (source, format!("/data/{source}/clip.flac")),
    )
    .expect("segment");
    let audio_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO transcript_segments (audio_segment_id, start_utc, end_utc, text)
         VALUES (?1, '2026-07-03T09:51:00+00:00', '2026-07-03T09:51:05+00:00', 'hello')",
        [audio_id],
    )
    .expect("turn");
    let turn_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO transcript_embeddings (segment_id) VALUES (?1)",
        [turn_id],
    )
    .expect("embedding");
    conn.execute(
        "INSERT INTO transcript_lineage (derived_id, source_id) VALUES (?1, 99)",
        [turn_id],
    )
    .expect("lineage");
    conn.execute(
        "INSERT INTO corrections (audio_segment_id, transcript_segment_id, corrected_text)
         VALUES (?1, ?2, 'fixed')",
        (audio_id, turn_id),
    )
    .expect("correction");
    conn.execute(
        "INSERT INTO refine_requests (source_id, start_utc, end_utc, created_utc)
         VALUES (?1, '2026-07-03T09:50:00+00:00', '2026-07-03T10:20:00+00:00', 'x')",
        [source],
    )
    .expect("refine");
    audio_id
}

fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .expect("count")
}

#[test]
fn deleting_the_household_archive_is_refused_and_removes_nothing() {
    // ⚠ The continuous capture is append-only; without this guard the household
    // archive is one HTTP call from gone.
    let mut conn = delete_db();
    populate(&conn, "usb", "coreaudio");

    let err = delete_session(&mut conn, "usb", NOW).expect_err("must refuse");

    assert!(matches!(err, SessionError::NotAnUpload), "got {err:?}");
    assert_eq!(count(&conn, "sources"), 1);
    assert_eq!(count(&conn, "audio_segments"), 1);
    assert_eq!(count(&conn, "transcript_segments"), 1);
    assert_eq!(count(&conn, "deleted_segments"), 0, "not even tombstoned");
}

#[test]
fn deleting_a_meeting_removes_every_derived_row_and_returns_its_files() {
    let mut conn = delete_db();
    populate(&conn, "meeting-1", "upload");

    let paths = delete_session(&mut conn, "meeting-1", NOW).expect("deleted");

    assert_eq!(paths, vec!["/data/meeting-1/clip.flac".to_owned()]);
    for table in [
        "sources",
        "audio_segments",
        "transcript_segments",
        "transcript_embeddings",
        "transcript_lineage",
        "corrections",
        "refine_requests",
    ] {
        assert_eq!(count(&conn, table), 0, "{table} still has rows");
    }
}

#[test]
fn a_deletion_is_tombstoned_so_a_later_push_cannot_resurrect_it() {
    // Without the tombstone the turns pass rebuilds the session, and a deletion
    // that undoes itself is worse than none.
    let mut conn = delete_db();
    populate(&conn, "meeting-1", "upload");

    delete_session(&mut conn, "meeting-1", NOW).expect("deleted");

    let (source, start): (String, String) = conn
        .query_row(
            "SELECT source_id, start_utc FROM deleted_segments",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("tombstone");
    assert_eq!(source, "meeting-1");
    assert_eq!(start, "2026-07-03T09:50:00+00:00");
}

#[test]
fn deleting_one_meeting_leaves_another_untouched() {
    let mut conn = delete_db();
    populate(&conn, "meeting-1", "upload");
    populate(&conn, "meeting-2", "upload");

    delete_session(&mut conn, "meeting-1", NOW).expect("deleted");

    assert_eq!(count(&conn, "sources"), 1);
    assert_eq!(count(&conn, "transcript_segments"), 1);
    let left: String = conn
        .query_row("SELECT id FROM sources", [], |r| r.get(0))
        .expect("row");
    assert_eq!(left, "meeting-2");
}

#[test]
fn deleting_a_session_that_does_not_exist_is_a_miss_not_a_wipe() {
    let mut conn = delete_db();
    populate(&conn, "meeting-1", "upload");

    let err = delete_session(&mut conn, "meeting-nope", NOW).expect_err("must fail");

    assert!(matches!(err, SessionError::Missing), "got {err:?}");
    assert_eq!(count(&conn, "sources"), 1);
}

#[test]
fn a_failed_delete_leaves_the_session_whole() {
    // Atomic: a half-deleted session is turns with no source, which no view can
    // render and no path can clean up.
    let mut conn = delete_db();
    populate(&conn, "meeting-1", "upload");
    conn.execute("DROP TABLE refine_requests", [])
        .expect("drop");

    let failed = delete_session(&mut conn, "meeting-1", NOW);

    assert!(failed.is_err());
    assert_eq!(count(&conn, "sources"), 1, "rolled back");
    assert_eq!(count(&conn, "transcript_segments"), 1);
    assert_eq!(
        count(&conn, "deleted_segments"),
        0,
        "including the tombstone"
    );
}
