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

// --- the instant feed --------------------------------------------------------

use recalld::work::{LiveTurn, ingest_live};

/// ⚠ The REAL migration ladder, not a slice of it. A hand-written copy stood
/// here and stopped matching production the first time a column was added.
fn live_store() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    recalld::meaning_schema::ensure(&conn).expect("schema");
    conn
}

/// Any delivery instant; only the test that asserts on it cares which.
fn delivered() -> chrono::DateTime<chrono::Utc> {
    "2026-09-09T10:00:31.500000+00:00".parse().unwrap()
}

fn a_turn(start: &str, text: &str) -> LiveTurn {
    LiveTurn {
        start: start.to_owned(),
        end: "2026-09-09T10:00:05+00:00".to_owned(),
        text: text.to_owned(),
        asr_model: "live".to_owned(),
        language: Some("en".to_owned()),
    }
}

/// ⚠ A live turn's `start_utc` is WHERE IN THE AUDIO the words were said, not
/// when the tier delivered them. Without `created_utc` the one tier whose whole
/// value is immediacy leaves no evidence of its own latency, and a stall can
/// only be caught while it is happening — which is how #1383's went unseen for
/// forty minutes.
#[test]
fn a_live_turn_records_when_it_was_delivered_not_only_when_it_was_said() {
    let mut conn = live_store();

    ingest_live(
        &mut conn,
        &[a_turn("2026-09-09T10:00:00+00:00", "hello there")],
        delivered(),
    )
    .unwrap();

    let (said, stored): (String, String) = conn
        .query_row(
            "SELECT start_utc, created_utc FROM transcript_segments",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("the turn carries both instants");
    assert_eq!(said, "2026-09-09T10:00:00+00:00");
    assert_eq!(
        stored, "2026-09-09T10:00:31.500000+00:00",
        "the delivery instant is what makes live latency measurable after the fact"
    );
}

/// ⚠ The search index has NO trigger behind it — `transcript_fts` is a
/// contentless FTS5 table the writer fills by hand. Forgetting it fails nothing
/// and makes every live turn unfindable by search, which is exactly what a live
/// turn is most likely to be looked up by.
#[test]
fn a_stored_live_turn_is_searchable() {
    let mut conn = live_store();

    let stored = ingest_live(
        &mut conn,
        &[a_turn("2026-09-09T10:00:00+00:00", "hello there")],
        delivered(),
    )
    .unwrap();

    assert_eq!(stored, 1);
    let found: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM transcript_fts WHERE transcript_fts MATCH 'hello'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(found, 1, "the turn exists but cannot be searched for");
}

/// ⚠ A re-push must never duplicate a turn NOR resurrect one the archive has
/// already reconciled to hidden.
#[test]
fn a_repushed_turn_is_skipped_even_once_hidden() {
    let mut conn = live_store();
    let turns = [a_turn("2026-09-09T10:00:00+00:00", "same words")];

    assert_eq!(ingest_live(&mut conn, &turns, delivered()).unwrap(), 1);
    assert_eq!(
        ingest_live(&mut conn, &turns, delivered()).unwrap(),
        0,
        "a retry duplicated it"
    );

    // The archive reconciles it away; a later retry must still not bring it back.
    conn.execute(
        "UPDATE transcript_segments SET hidden_reason = 'reconciled'",
        [],
    )
    .unwrap();

    assert_eq!(
        ingest_live(&mut conn, &turns, delivered()).unwrap(),
        0,
        "a hidden turn was resurrected"
    );
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM transcript_segments", [], |r| r.get(0))
        .unwrap();
    assert_eq!(total, 1);
}

/// ⚠ The stored spelling is what the presence check compares. A turn re-spelled
/// on the way in would never match its own earlier copy, and every retry would
/// insert again — the duplicate-forever bug.
#[test]
fn a_z_suffixed_time_matches_the_offset_spelling_it_was_stored_as() {
    let mut conn = live_store();

    assert_eq!(
        ingest_live(
            &mut conn,
            &[a_turn("2026-09-09T10:00:00+00:00", "x")],
            delivered()
        )
        .unwrap(),
        1
    );
    // The same instant, spelled the other way round.
    assert_eq!(
        ingest_live(
            &mut conn,
            &[a_turn("2026-09-09T10:00:00Z", "x")],
            delivered()
        )
        .unwrap(),
        0
    );

    let stored: String = conn
        .query_row("SELECT start_utc FROM transcript_segments", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        stored, "2026-09-09T10:00:00+00:00",
        "Z must be stored as +00:00"
    );
}

#[test]
fn an_unparseable_time_costs_that_turn_and_no_other() {
    let mut conn = live_store();

    let stored = ingest_live(
        &mut conn,
        &[
            a_turn("not a time", "dropped"),
            a_turn("2026-09-09T10:00:00+00:00", "kept"),
        ],
        delivered(),
    )
    .unwrap();

    assert_eq!(stored, 1, "one bad turn must not cost the batch");
    let text: String = conn
        .query_row("SELECT text FROM transcript_segments", [], |r| r.get(0))
        .unwrap();
    assert_eq!(text, "kept");
}

// --- the audio blob plane ----------------------------------------------------

use recalld::sync::safe_component;

/// ⚠ THE path-traversal guard. The Mac is authenticated, but a compromised token
/// must not become an arbitrary file write anywhere on the fleet's disk.
#[test]
fn a_component_that_could_escape_the_archive_is_refused() {
    for hostile in [
        "",          // would collapse the path
        "..",        // the parent
        "../etc",    // the classic
        "a/../../b", // .. anywhere, not just at the start
        "a/b",       // a separator makes it two components
        "a\\b",      // and the Windows one, which some filesystems honour
        ".hidden",   // a leading dot lands a push as a dotfile
        ".",
    ] {
        assert_eq!(safe_component(hostile), None, "accepted {hostile:?}");
    }
}

/// ⚠ And it must accept what actually flows, or the sync stops. These are real
/// names from the archive.
#[test]
fn the_names_the_mac_really_pushes_are_accepted() {
    for real in [
        "usb-20260613T170653.opus",
        "meeting-20260520-1901-20260520T180121.mp3",
        "pixel9-20260903T110000.flac",
        "usb",
        "meeting-20260520-1901",
    ] {
        assert_eq!(safe_component(real), Some(real), "refused {real:?}");
    }
}

/// ⚠ **The reason this guard is `safe_component` EXPIRED on 2026-09-17.** It was
/// `safe_component` and not `audiocore::names::parse` because that grammar took
/// only flac/opus/ogg/wav while every uploaded meeting is `.mp3` — the strict
/// check would have 400'd every meeting audio push and the Mac would have
/// retried for ever. The grammar now covers those containers (#1649), so the
/// strict check is available here and would additionally pin the stamp. Left as
/// it is because this push path goes with `sync_push` (#1538); tightening
/// something on its way out buys nothing.
#[test]
fn a_real_meeting_file_now_passes_the_strict_segment_grammar_too() {
    let meeting = "meeting-20260520-1901-20260520T180121.mp3";

    let parsed = audiocore::names::parse("meeting-20260520-1901", meeting)
        .expect("the grammar covers an uploaded meeting's container");
    assert_eq!(parsed.start_utc, "2026-05-20T18:01:21Z");
    assert_eq!(safe_component(meeting), Some(meeting));
}

/// ⚠ A live turn is short and hard, which is exactly what Whisper loops on. The
/// filter is here rather than in the pusher because whether a string is a model
/// artefact is a property of the STRING — so every writer gets the same answer.
#[test]
fn a_degenerate_loop_is_not_stored_as_a_live_turn() {
    let mut conn = live_store();

    assert_eq!(
        ingest_live(
            &mut conn,
            &[a_turn(
                "2026-09-09T10:00:00+00:00",
                "goog goog goog goog goog goog"
            )],
            delivered(),
        )
        .unwrap(),
        0
    );
    assert_eq!(
        ingest_live(
            &mut conn,
            &[a_turn("2026-09-09T10:00:01+00:00", "... ***")],
            delivered()
        )
        .unwrap(),
        0
    );
    // And real speech still lands, so the filter is not simply refusing.
    assert_eq!(
        ingest_live(
            &mut conn,
            &[a_turn(
                "2026-09-09T10:00:02+00:00",
                "we should leave at eight"
            )],
            delivered(),
        )
        .unwrap(),
        1
    );
}

/// ⚠ **The ASR prompt lists household names first, so on audio it cannot place
/// the model reaches for them** — eight scripted lines containing no names
/// produced a household first name four times, and across every short turn ever
/// written the live tier is 7x likelier than the archive pass to emit a turn
/// that is nothing but a name (#1665). A false name is invisible to every other
/// signal: fluent, Latin script, correctly language-labelled, plausibly timed.
#[test]
fn a_live_turn_that_is_nothing_but_a_household_name_is_refused() {
    let mut conn = live_store();
    conn.execute("INSERT INTO speakers (name) VALUES ('Anna')", [])
        .unwrap();

    let stored = ingest_live(
        &mut conn,
        &[
            a_turn("2026-09-09T10:00:00+00:00", "Anna."),
            a_turn("2026-09-09T10:00:01+00:00", " anna "),
            a_turn("2026-09-09T10:00:02+00:00", "Anna, are you there?"),
            a_turn("2026-09-09T10:00:03+00:00", "Annabel"),
        ],
        delivered(),
    )
    .unwrap();

    // ⚠ Only the BARE name goes. A name inside a sentence is the ordinary case
    // this system exists to record, and a different name that merely starts the
    // same way is a different word.
    assert_eq!(stored, 2, "a name in a sentence must survive");
    let kept: Vec<String> = conn
        .prepare("SELECT text FROM transcript_segments ORDER BY start_utc")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(kept, vec!["Anna, are you there?", "Annabel"]);
}

/// ⚠ The refusal is scoped to a name the household actually has. Without an
/// enrolled speaker it must do nothing at all — a rule that fires on an empty
/// list would silently refuse whatever happened to look like one.
#[test]
fn with_nobody_enrolled_the_bare_name_rule_refuses_nothing() {
    let mut conn = live_store();
    let stored = ingest_live(
        &mut conn,
        &[a_turn("2026-09-09T10:00:00+00:00", "Anna.")],
        delivered(),
    )
    .unwrap();
    assert_eq!(stored, 1);
}
