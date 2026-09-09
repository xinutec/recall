//! The sync plane's read routes, and their parity with the Python they replace.

use recalld::labels::{cluster_namings, initial_prompt};
use rusqlite::Connection;

/// The meaning-plane tables these reads touch.
fn store() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE speakers (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE);
         CREATE TABLE vocabulary (
             id INTEGER PRIMARY KEY, term TEXT NOT NULL UNIQUE,
             created_utc TEXT NOT NULL);
         CREATE TABLE audio_segments (
             id INTEGER PRIMARY KEY, source_id TEXT NOT NULL);
         CREATE TABLE transcript_segments (
             id INTEGER PRIMARY KEY, audio_segment_id INTEGER,
             speaker_label TEXT, speaker_cluster TEXT,
             superseded_by INTEGER, hidden_reason TEXT);",
    )
    .unwrap();
    conn
}

fn glossary(conn: &Connection, speakers: &[&str], terms: &[&str]) {
    for name in speakers {
        conn.execute("INSERT INTO speakers (name) VALUES (?1)", [name])
            .unwrap();
    }
    for term in terms {
        conn.execute(
            "INSERT INTO vocabulary (term, created_utc) VALUES (?1, '2026-09-09T00:00:00+00:00')",
            [term],
        )
        .unwrap();
    }
}

/// ⚠ The expected string is the PYTHON's output, not this implementation's:
/// `recall.vocabulary.build_initial_prompt` was run on exactly these rows on
/// 2026-09-09 and printed it.
///
/// Three rules are pinned at once, and each was a real chance to diverge:
/// speaker names come BEFORE the vocabulary; a term appearing in both is
/// carried once, at its FIRST position; and the ordering inside each group is
/// `COLLATE NOCASE`, not insertion order.
#[test]
fn the_glossary_prompt_matches_what_the_python_built() {
    let conn = store();
    glossary(
        &conn,
        &["Pippijn", "Michiel", "Zebra"],
        &[
            "apple",
            "Michiel",
            "banana",
            &"x".repeat(580),
            "never-reached",
        ],
    );

    assert_eq!(
        initial_prompt(&conn).unwrap().as_deref(),
        Some("Michiel, Pippijn, Zebra, apple, banana, never-reached")
    );
}

/// ⚠ The cap BREAKS the list, it does not skip past the long term and carry on.
/// Skipping would make the prompt depend on which terms happen to be long rather
/// than on their priority, and would silently reorder what the model is biased
/// toward. The 580-char term above sorts LAST, which is why `never-reached`
/// survives — put a long term early and everything after it must vanish.
#[test]
fn a_term_over_the_cap_ends_the_list_rather_than_being_skipped() {
    let conn = store();
    // The middle term overflows the cap ON ITS OWN (3 + 2 + 700 = 705), so the
    // two behaviours are distinguishable: BREAK gives "aaa"; SKIP would give
    // "aaa, ccc", which fits easily and is the wrong answer.
    glossary(&conn, &[], &["aaa", &"b".repeat(700), "ccc"]);

    let prompt = initial_prompt(&conn).unwrap().expect("a prompt");

    assert_eq!(
        prompt, "aaa",
        "the list must END at the first term over the cap, not skip it and carry on"
    );
}

#[test]
fn an_empty_glossary_is_none_not_an_empty_string() {
    // The Mac branches on null; "" would read as a prompt that biases nothing.
    assert_eq!(initial_prompt(&store()).unwrap(), None);
}

// --- cluster namings ---------------------------------------------------------

fn turn(conn: &Connection, id: i64, source: &str, cluster: &str, label: Option<&str>) {
    conn.execute(
        "INSERT OR IGNORE INTO audio_segments (id, source_id) VALUES (?1, ?2)",
        rusqlite::params![id, source],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO transcript_segments (audio_segment_id, speaker_cluster, speaker_label)
         VALUES (?1, ?2, ?3)",
        rusqlite::params![id, cluster, label],
    )
    .unwrap();
}

/// ⚠ A cluster is ONE voice, so the DOMINANT label wins. Without this a voice
/// whose turns were individually relabelled a few times would send the Mac
/// several contradictory names for the same cluster, and `name_voice` would
/// replay whichever arrived last.
#[test]
fn the_dominant_label_wins_for_a_cluster() {
    let conn = store();
    // Three turns say one name, one turn says another — same source, same voice.
    turn(&conn, 1, "usb", "c1", Some("Pippijn"));
    turn(&conn, 2, "usb", "c1", Some("Pippijn"));
    turn(&conn, 3, "usb", "c1", Some("Pippijn"));
    turn(&conn, 4, "usb", "c1", Some("Michiel"));

    let named = cluster_namings(&conn).unwrap();

    assert_eq!(named.len(), 1, "one voice, one mapping");
    assert_eq!(named[0].name, "Pippijn");
    assert_eq!(named[0].source_id, "usb");
    assert_eq!(named[0].cluster, "c1");
}

#[test]
fn unnamed_superseded_and_hidden_turns_are_not_labels() {
    let conn = store();
    turn(&conn, 1, "usb", "c1", None); // never named
    turn(&conn, 2, "usb", "c2", Some("Ghost"));
    conn.execute(
        "UPDATE transcript_segments SET superseded_by = 99 WHERE id = 2",
        [],
    )
    .unwrap();
    turn(&conn, 3, "usb", "c3", Some("Hidden"));
    conn.execute(
        "UPDATE transcript_segments SET hidden_reason = 'no words' WHERE id = 3",
        [],
    )
    .unwrap();

    assert!(cluster_namings(&conn).unwrap().is_empty());
}

/// The payload's ORDER is part of it: the Mac diffs the whole set each pass, so
/// an unstable order would read as churn.
#[test]
fn the_payload_is_ordered_by_source_then_cluster() {
    let conn = store();
    turn(&conn, 1, "usb", "c2", Some("Second"));
    turn(&conn, 2, "geb", "c1", Some("First"));
    turn(&conn, 3, "usb", "c1", Some("Middle"));

    let named = cluster_namings(&conn).unwrap();
    let keys: Vec<(&str, &str)> = named
        .iter()
        .map(|n| (n.source_id.as_str(), n.cluster.as_str()))
        .collect();

    assert_eq!(keys, vec![("geb", "c1"), ("usb", "c1"), ("usb", "c2")]);
}

/// ⚠ `snake_case` ON THE WIRE. The Python model declares these bare and the Mac's
/// client parses them bare, so a camelCase "tidy-up" here drops every label at
/// the Mac — silently, because the payload would still be valid JSON.
#[test]
fn the_label_wire_shape_is_snake_case() {
    let conn = store();
    turn(&conn, 1, "usb", "c1", Some("Pippijn"));

    let json = serde_json::to_string(&cluster_namings(&conn).unwrap()).unwrap();

    assert_eq!(
        json,
        r#"[{"source_id":"usb","cluster":"c1","name":"Pippijn"}]"#
    );
}

// --- the job queue -----------------------------------------------------------

use recalld::work::{mark_refine_done, mark_transcribed, pending_jobs};

fn queue_store() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE sources (id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL);
         CREATE TABLE refine_requests (
             id INTEGER PRIMARY KEY AUTOINCREMENT, source_id TEXT NOT NULL,
             start_utc TEXT NOT NULL, end_utc TEXT NOT NULL,
             created_utc TEXT NOT NULL, done_utc TEXT);
         CREATE TABLE audio_segments (
             id INTEGER PRIMARY KEY, source_id TEXT NOT NULL, path TEXT NOT NULL,
             start_utc TEXT NOT NULL, end_utc TEXT NOT NULL,
             sample_rate INTEGER NOT NULL, channels INTEGER NOT NULL,
             transcribed_utc TEXT);",
    )
    .expect("schema");
    conn.execute_batch(
        "INSERT INTO sources VALUES ('up','A Meeting','upload'), ('usb','usb','coreaudio');
         INSERT INTO refine_requests (source_id,start_utc,end_utc,created_utc)
           VALUES ('usb','2026-09-01T10:00:00+00:00','2026-09-01T10:05:00+00:00','2026-09-01T10:00:00+00:00');
         INSERT INTO refine_requests (source_id,start_utc,end_utc,created_utc,done_utc)
           VALUES ('usb','2026-09-01T11:00:00+00:00','2026-09-01T11:05:00+00:00','2026-09-01T11:00:00+00:00','2026-09-01T12:00:00+00:00');
         INSERT INTO audio_segments (id,source_id,path,start_utc,end_utc,sample_rate,channels)
           VALUES (1,'up','/deep/nested/dir/clip-0.flac','2026-09-02T09:00:00+00:00','2026-09-02T09:10:00+00:00',48000,1),
                  (2,'up','/deep/nested/dir/clip-1.flac','2026-09-02T08:00:00+00:00','2026-09-02T08:10:00+00:00',48000,1);",
    )
    .expect("fixture");
    conn
}

/// ⚠ The expected payload is the PYTHON's — `_job_of` / `_upload_job_of` were run
/// on exactly these rows on 2026-09-09 and printed it.
///
/// Four rules ride on this one assertion, and each was a chance to diverge:
/// refines come FIRST, a done refine is absent, uploads order by START TIME (so
/// id 2 precedes id 1 here), and `file` is the BASENAME of a nested path.
#[test]
fn the_job_queue_matches_what_the_python_served() {
    let conn = queue_store();

    let jobs = pending_jobs(&conn, 50).unwrap();
    let json = serde_json::to_value(&jobs).unwrap();

    assert_eq!(
        json,
        serde_json::json!([
            {"id":1,"type":"refine","source":"usb",
             "start":"2026-09-01T10:00:00+00:00","end":"2026-09-01T10:05:00+00:00",
             "file":null,"title":null,"sample_rate":null,"channels":null},
            {"id":2,"type":"upload","source":"up",
             "start":"2026-09-02T08:00:00+00:00","end":"2026-09-02T08:10:00+00:00",
             "file":"clip-1.flac","title":"A Meeting","sample_rate":48000,"channels":1},
            {"id":1,"type":"upload","source":"up",
             "start":"2026-09-02T09:00:00+00:00","end":"2026-09-02T09:10:00+00:00",
             "file":"clip-0.flac","title":"A Meeting","sample_rate":48000,"channels":1},
        ])
    );
}

/// ⚠ The two queues share ONE limit and refines take it first — a backlog of
/// uploads must never starve a refine somebody is waiting on in the UI.
#[test]
fn refines_take_the_limit_before_uploads_do() {
    let conn = queue_store();

    let jobs = pending_jobs(&conn, 1).unwrap();

    assert_eq!(jobs.len(), 1);
    assert_eq!(
        jobs[0].r#type, "refine",
        "the refine must win the only slot"
    );
}

/// ⚠ `transcribed_utc` takes the segment's own `end_utc`, NOT the current time.
/// The column reads as "the recording this covers ended then", so anything
/// ordering or ageing by it stays on the RECORDING's clock. Writing `now` makes a
/// months-old backlog look like it was all recorded the day it drained.
#[test]
fn retiring_an_upload_stamps_the_recordings_end_not_the_clock() {
    let conn = queue_store();

    mark_transcribed(&conn, 1).unwrap();

    let (end, transcribed): (String, String) = conn
        .query_row(
            "SELECT end_utc, transcribed_utc FROM audio_segments WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(transcribed, end);
    assert_eq!(transcribed, "2026-09-02T09:10:00+00:00");
    // And it leaves the queue.
    assert!(
        !pending_jobs(&conn, 50)
            .unwrap()
            .iter()
            .any(|j| j.r#type == "upload" && j.id == 1)
    );
}

#[test]
fn retiring_a_refine_removes_it_from_the_queue() {
    let conn = queue_store();
    let at = chrono::DateTime::from_timestamp(1_788_998_400, 0).unwrap();

    mark_refine_done(&conn, 1, at).unwrap();

    assert!(
        !pending_jobs(&conn, 50)
            .unwrap()
            .iter()
            .any(|j| j.r#type == "refine")
    );
}
