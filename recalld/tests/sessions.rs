//! Uploaded meetings: the list, the guards, and the transcript export.
//!
//! ⚠ The guards are the point. Rename and re-diarize reach the sources table,
//! and the household capture archive must not be reachable through a path meant
//! for meetings.

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
            superseded_by INTEGER, hidden_reason TEXT);
         CREATE TABLE refine_requests (
            id INTEGER PRIMARY KEY AUTOINCREMENT, source_id TEXT NOT NULL,
            start_utc TEXT NOT NULL, end_utc TEXT NOT NULL,
            created_utc TEXT NOT NULL, done_utc TEXT);",
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
    // SPEAKER_00 is an answer about voices, not about people. Listing it would
    // put a machine label where a name goes.
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
    // transcribes it. If it did not, an upload would look like it failed.
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

#[test]
fn rediarizing_queues_the_whole_recording_and_runs_nothing_inline() {
    let conn = db();
    source(&conn, "meeting-1", "upload");
    segment(
        &conn,
        "meeting-1",
        "2026-07-03T09:50:00+00:00",
        "2026-07-03T10:00:00+00:00",
    );
    segment(
        &conn,
        "meeting-1",
        "2026-07-03T10:00:00+00:00",
        "2026-07-03T10:20:00+00:00",
    );

    rediarize(&conn, "meeting-1", NOW).expect("queued");

    let (start, end): (String, String) = conn
        .query_row(
            "SELECT start_utc, end_utc FROM refine_requests WHERE source_id = 'meeting-1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("one request");
    assert_eq!(
        start, "2026-07-03T09:50:00+00:00",
        "spans from the FIRST segment"
    );
    assert_eq!(end, "2026-07-03T10:20:00+00:00", "to the LAST");
}

#[test]
fn rediarizing_a_session_with_no_audio_says_so() {
    let conn = db();
    source(&conn, "meeting-1", "upload");

    let err = rediarize(&conn, "meeting-1", NOW).expect_err("nothing to refine");

    assert!(matches!(err, SessionError::NoAudio), "got {err:?}");
}

#[test]
fn rediarizing_the_household_archive_is_refused() {
    let conn = db();
    source(&conn, "usb", "coreaudio");
    segment(
        &conn,
        "usb",
        "2026-07-03T09:00:00+00:00",
        "2026-07-03T11:00:00+00:00",
    );

    let err = rediarize(&conn, "usb", NOW).expect_err("must refuse");

    assert!(matches!(err, SessionError::NotAnUpload), "got {err:?}");
    let queued: i64 = conn
        .query_row("SELECT COUNT(*) FROM refine_requests", [], |r| r.get(0))
        .expect("count");
    assert_eq!(queued, 0, "a refused guard queues nothing");
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
    // ⚠ Falling back to "unknown" for both would MERGE two different people into
    // one bubble — the export would read as one person saying both halves.
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
