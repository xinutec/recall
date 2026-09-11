//! The segment push — the one route whose failure destroys data rather than
//! reporting it. Every test here is paired with an ablation in the commit that
//! added it: a rule that can be removed with these still green is not covered.

use recalld::work::{SegmentIn, SegmentStoredOut, TurnIn, ingest_segment};
use rusqlite::Connection;

/// ⚠ THE REAL SCHEMA, copied from the fleet's own `sqlite_master`. An invented
/// one is why the first deploy of this route 500'd on every push with "table
/// sources has no column named spec": the test had a column the database does
/// not, taken from the Python DATACLASS rather than the table. A fixture that
/// mirrors the wiring tests its own copy.
///
/// One constant, because the route tests below need the same tables: a second
/// copy is a second thing to correct when the fleet's schema moves.
const SCHEMA: &str =
    "CREATE TABLE sources (id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL,
             port INTEGER, event_db REAL, noise_shape BLOB);
         CREATE TABLE audio_segments (
             id INTEGER PRIMARY KEY, source_id TEXT NOT NULL, path TEXT NOT NULL,
             start_utc TEXT NOT NULL, end_utc TEXT NOT NULL,
             sample_rate INTEGER NOT NULL, channels INTEGER NOT NULL,
             transcribed_utc TEXT, UNIQUE (source_id, start_utc));
         CREATE TABLE transcript_segments (
             id INTEGER PRIMARY KEY, audio_segment_id INTEGER, start_utc TEXT NOT NULL,
             end_utc TEXT NOT NULL, text TEXT NOT NULL, language TEXT,
             asr_confidence REAL, asr_model TEXT NOT NULL, speaker_cluster TEXT,
             speaker_guess TEXT, speaker_score REAL, provenance TEXT,
             superseded_by INTEGER, hidden_reason TEXT);
         CREATE VIRTUAL TABLE transcript_fts USING fts5(text, content='');
         CREATE TABLE corrections (
             id INTEGER PRIMARY KEY, audio_segment_id INTEGER NOT NULL,
             start_utc TEXT NOT NULL, end_utc TEXT NOT NULL,
             corrected_text TEXT NOT NULL, language TEXT);
         CREATE TABLE deleted_segments (source_id TEXT NOT NULL, start_utc TEXT NOT NULL);";

fn store() -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().expect("tmp");
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(SCHEMA).expect("schema");
    (dir, conn)
}

fn turn(start: &str, end: &str, text: &str) -> TurnIn {
    TurnIn {
        start: format!("2026-09-09T10:{start}+00:00"),
        end: format!("2026-09-09T10:{end}+00:00"),
        text: text.to_owned(),
        asr_model: "turbo".to_owned(),
        language: Some("en".to_owned()),
        asr_confidence: Some(0.9),
        speaker_cluster: Some("c1".to_owned()),
        speaker_guess: None,
        speaker_score: None,
        provenance: None,
    }
}

fn segment(turns: Vec<TurnIn>) -> SegmentIn {
    SegmentIn {
        source_id: "usb".to_owned(),
        source_name: "USB mic".to_owned(),
        kind: "coreaudio".to_owned(),
        // Absolute on the MAC — the fleet must not store this verbatim.
        path: "/Volumes/Backup/recall/usb/usb-20260909T100000.opus".to_owned(),
        start: "2026-09-09T10:00:00+00:00".to_owned(),
        end: "2026-09-09T10:01:00+00:00".to_owned(),
        sample_rate: 48000,
        channels: 1,
        turns,
    }
}

fn push(conn: &mut Connection, root: &std::path::Path, seg: &SegmentIn) -> SegmentStoredOut {
    ingest_segment(conn, seg, root).expect("ingest")
}

fn visible(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare(
            "SELECT text FROM transcript_segments \
             WHERE superseded_by IS NULL AND hidden_reason IS NULL ORDER BY start_utc",
        )
        .unwrap();
    let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
    rows.map(Result::unwrap).collect()
}

// --- RULE 5: the one that destroys data --------------------------------------

/// ⚠ **THE test on this route.** A machine turn overlapping a HUMAN CORRECTION
/// must be skipped. Without it the next sync push overwrites somebody's typed
/// ground truth, returns 200, and nothing records that it happened — the exact
/// corrections #1461 asks a person to spend half an hour making.
#[test]
fn a_machine_turn_overlapping_a_human_correction_is_not_written() {
    let (dir, mut conn) = store();
    push(
        &mut conn,
        dir.path(),
        &segment(vec![turn("00:00", "00:10", "machine first pass")]),
    );
    let audio_id: i64 = conn
        .query_row("SELECT id FROM audio_segments", [], |r| r.get(0))
        .unwrap();
    // A person corrected 00:00–00:10.
    conn.execute(
        "INSERT INTO corrections (audio_segment_id, start_utc, end_utc, corrected_text) \
         VALUES (?1, '2026-09-09T10:00:00+00:00', '2026-09-09T10:00:10+00:00', 'what was really said')",
        [audio_id],
    )
    .unwrap();

    // A newer machine pass covers the same span, plus a span nobody corrected.
    let out = push(
        &mut conn,
        dir.path(),
        &segment(vec![
            turn("00:00", "00:10", "machine SECOND pass"),
            turn("00:20", "00:30", "uncorrected span"),
        ]),
    );

    assert_eq!(
        out.turns_written, 1,
        "the corrected span must not be rewritten"
    );
    // ⚠ The PYTHON's exact answer for this fixture, run 2026-09-09: one turn
    // written, and the only thing visible is the span nobody corrected. Note
    // what is NOT here — "machine SECOND pass" was refused by the human overlap,
    // and "machine first pass" was superseded by the newer pass. Asserting the
    // whole list rather than two facts about it is what catches a turn left
    // visible that should have been hidden.
    assert_eq!(
        visible(&conn),
        vec!["uncorrected span"],
        "diverged from the Python on the same fixture"
    );
}

/// The overlap is STRICT: a turn that merely touches a correction's boundary is
/// not overlapping it, and must still be written.
#[test]
fn a_turn_abutting_a_correction_is_still_written() {
    let (dir, mut conn) = store();
    push(
        &mut conn,
        dir.path(),
        &segment(vec![turn("00:00", "00:10", "first")]),
    );
    let audio_id: i64 = conn
        .query_row("SELECT id FROM audio_segments", [], |r| r.get(0))
        .unwrap();
    conn.execute(
        "INSERT INTO corrections (audio_segment_id, start_utc, end_utc, corrected_text) \
         VALUES (?1, '2026-09-09T10:00:00+00:00', '2026-09-09T10:00:10+00:00', 'human')",
        [audio_id],
    )
    .unwrap();

    // Starts exactly where the correction ends.
    let out = push(
        &mut conn,
        dir.path(),
        &segment(vec![turn("00:10", "00:20", "abutting")]),
    );

    assert_eq!(out.turns_written, 1);
    assert!(visible(&conn).iter().any(|t| t == "abutting"));
}

// --- RULE 1: the deletion veto -----------------------------------------------

/// ⚠ A deliberately deleted identity must be REFUSED, not re-stored — and the
/// blob a racing audio push landed first must go with it.
#[test]
fn a_tombstoned_identity_is_refused_and_its_racing_blob_removed() {
    let (dir, mut conn) = store();
    conn.execute(
        "INSERT INTO deleted_segments VALUES ('usb', '2026-09-09T10:00:00+00:00')",
        [],
    )
    .unwrap();
    // A racing audio push landed the blob first.
    let blob = dir.path().join("usb").join("usb-20260909T100000.opus");
    std::fs::create_dir_all(blob.parent().unwrap()).unwrap();
    std::fs::write(&blob, b"bytes").unwrap();

    let out = push(
        &mut conn,
        dir.path(),
        &segment(vec![turn("00:00", "00:10", "resurrected")]),
    );

    assert!(out.tombstoned, "a deleted identity was accepted");
    assert_eq!(out.audio_segment_id, 0);
    assert_eq!(out.turns_written, 0);
    assert!(visible(&conn).is_empty(), "a deleted segment came back");
    assert!(!blob.exists(), "the racing blob survived the refusal");
}

// --- RULE 2: the path is re-homed --------------------------------------------

/// ⚠ Storing the sender's absolute path gave the fleet a database describing a
/// filesystem it cannot see: transcripts read perfectly and every play button
/// 404s, silently, for ever.
#[test]
fn the_stored_path_is_the_fleets_own_not_the_macs() {
    let (dir, mut conn) = store();

    push(
        &mut conn,
        dir.path(),
        &segment(vec![turn("00:00", "00:10", "x")]),
    );

    let path: String = conn
        .query_row("SELECT path FROM audio_segments", [], |r| r.get(0))
        .unwrap();
    assert!(
        !path.contains("/Volumes/Backup"),
        "the Mac's path was stored: {path}"
    );
    assert_eq!(
        path,
        dir.path()
            .join("usb")
            .join("usb-20260909T100000.opus")
            .to_string_lossy()
    );
}

// --- RULE 3: live reconciliation ---------------------------------------------

/// ⚠ Runs on EVERY ingest, before the no-op check — so a live turn that arrived
/// after the segment was first stored is still swapped for the archive version
/// rather than shown beside it.
#[test]
fn live_turns_inside_the_span_are_reconciled_even_on_a_repush() {
    let (dir, mut conn) = store();
    let seg = segment(vec![turn("00:00", "00:10", "archive")]);
    push(&mut conn, dir.path(), &seg);

    // A live turn lands afterwards, inside the segment's span.
    conn.execute(
        "INSERT INTO transcript_segments (audio_segment_id, start_utc, end_utc, text, asr_model) \
         VALUES (NULL, '2026-09-09T10:00:05+00:00', '2026-09-09T10:00:08+00:00', 'provisional', 'live')",
        [],
    )
    .unwrap();
    assert!(visible(&conn).iter().any(|t| t == "provisional"));

    // The identical re-push writes nothing — and must STILL reconcile.
    let out = push(&mut conn, dir.path(), &seg);

    assert_eq!(out.turns_written, 0, "an identical re-push churned");
    assert!(
        !visible(&conn).iter().any(|t| t == "provisional"),
        "a live turn arriving after the segment was never reconciled"
    );
}

// --- RULE 4: the no-op -------------------------------------------------------

#[test]
fn an_identical_repush_writes_nothing_even_reordered() {
    let (dir, mut conn) = store();
    let first = segment(vec![
        turn("00:00", "00:10", "a"),
        turn("00:20", "00:30", "b"),
    ]);
    let out = push(&mut conn, dir.path(), &first);
    assert_eq!(out.turns_written, 2);

    // Same turns, opposite order: identity is a SORTED comparison.
    let reordered = segment(vec![
        turn("00:20", "00:30", "b"),
        turn("00:00", "00:10", "a"),
    ]);
    let again = push(&mut conn, dir.path(), &reordered);

    assert_eq!(
        again.turns_written, 0,
        "a reordered re-push rewrote the turns"
    );
    assert_eq!(visible(&conn).len(), 2);
}

/// A genuinely newer pass supersedes the old machine turns.
#[test]
fn a_newer_machine_pass_supersedes_the_old_turns() {
    let (dir, mut conn) = store();
    push(
        &mut conn,
        dir.path(),
        &segment(vec![turn("00:00", "00:10", "rough")]),
    );

    let out = push(
        &mut conn,
        dir.path(),
        &segment(vec![turn("00:00", "00:10", "refined")]),
    );

    assert_eq!(out.turns_written, 1);
    assert_eq!(
        visible(&conn),
        vec!["refined"],
        "both passes are showing at once"
    );
}

/// The sender owns the kind, so a correction on the Mac must reach the fleet.
#[test]
fn a_changed_kind_reaches_the_fleet_rather_than_sticking_at_the_first_one() {
    let (dir, mut conn) = store();
    push(
        &mut conn,
        dir.path(),
        &segment(vec![turn("00:00", "00:10", "x")]),
    );

    let mut corrected = segment(vec![turn("00:00", "00:10", "x")]);
    corrected.kind = "tcp_pcm".to_owned();
    corrected.source_name = "Renamed".to_owned();
    push(&mut conn, dir.path(), &corrected);

    let (kind, name): (String, String) = conn
        .query_row("SELECT kind, name FROM sources WHERE id = 'usb'", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(kind, "tcp_pcm", "the fleet kept the first kind it was told");
    // ⚠ The NAME is NOT taken from the sender. It is overwritten only while the
    // stored name is still the placeholder (equal to the id), so a name a person
    // set on the fleet survives every later push. Asserting "Renamed" here — as
    // this test first did — asserts a silent rename of somebody's title.
    assert_eq!(
        name, "USB mic",
        "a sync push renamed a source the fleet had named"
    );
}

/// The voiceprint guess rides along because the fleet has no ML to recompute it.
#[test]
fn the_speaker_guess_is_carried_since_the_fleet_cannot_recompute_it() {
    let (dir, mut conn) = store();
    let mut t = turn("00:00", "00:10", "x");
    t.speaker_guess = Some("Pippijn".to_owned());
    t.speaker_score = Some(0.82);

    push(&mut conn, dir.path(), &segment(vec![t]));

    let (guess, score): (Option<String>, Option<f64>) = conn
        .query_row(
            "SELECT speaker_guess, speaker_score FROM transcript_segments",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(guess.as_deref(), Some("Pippijn"));
    assert_eq!(score, Some(0.82));
}

/// A pushed turn must be findable — the index has no trigger behind it.
#[test]
fn a_pushed_turn_is_searchable() {
    let (dir, mut conn) = store();

    push(
        &mut conn,
        dir.path(),
        &segment(vec![turn("00:00", "00:10", "distinctive phrase")]),
    );

    let found: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM transcript_fts WHERE transcript_fts MATCH 'distinctive'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(found, 1);
}

// --- the routes themselves ---------------------------------------------------
//
// ⚠ Everything above tests `ingest_segment`. These test the ROUTES, and they
// exist because /sync/segments/batch — which carries ONE HUNDRED PERCENT of the
// real push traffic, the single route gets none — had no direct test at all
// until the Python it replaced was read line by line before deletion.

/// Mount the real router over a database with the real schema, and serve it.
async fn serve() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tmp");
    let root = dir.path().to_path_buf();
    recalld::store::open(&root).expect("ingest db");
    let conn = recalld::work::open_write(&root).expect("recall db");
    conn.execute_batch(SCHEMA).expect("schema");
    drop(conn);

    let app = recalld::app::router(std::sync::Arc::new(recalld::app::Config {
        root,
        tokens: None,
        read_token: None,
        max_body_bytes: recalld::app::DEFAULT_MAX_BODY,
        webauth: None,
        sync_token: Some("sekrit".to_owned()),
        upstream: None,
        frontend: None,
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr").to_string();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (dir, addr)
}

async fn post_json(addr: &str, path: &str, body: serde_json::Value) -> (u16, String) {
    let url = format!("http://{addr}{path}");
    tokio::task::spawn_blocking(move || {
        match ureq::post(&url)
            .set("Authorization", "Bearer sekrit")
            .send_json(body)
        {
            Ok(res) => (res.status(), res.into_string().unwrap_or_default()),
            Err(ureq::Error::Status(code, res)) => (code, res.into_string().unwrap_or_default()),
            Err(err) => panic!("transport: {err}"),
        }
    })
    .await
    .expect("request")
}

fn wire(seg: &SegmentIn) -> serde_json::Value {
    serde_json::json!({
        "source_id": seg.source_id, "source_name": seg.source_name, "kind": seg.kind,
        "path": seg.path, "start": seg.start, "end": seg.end,
        "sample_rate": seg.sample_rate, "channels": seg.channels,
        "turns": seg.turns.iter().map(|t| serde_json::json!({
            "start": t.start, "end": t.end, "text": t.text, "asr_model": t.asr_model,
            "language": t.language, "asr_confidence": t.asr_confidence,
            "speaker_cluster": t.speaker_cluster, "speaker_guess": t.speaker_guess,
            "speaker_score": t.speaker_score, "provenance": t.provenance,
        })).collect::<Vec<_>>(),
    })
}

fn stored_kind(root: &std::path::Path) -> Option<String> {
    let conn = recalld::work::open_write(root).expect("db");
    conn.query_row("SELECT kind FROM sources WHERE id = 'usb'", [], |r| {
        r.get::<_, String>(0)
    })
    .ok()
}

/// ⚠ **A parity divergence found by reading the Python before deleting it.** The
/// Python answers 400 for a `kind` no `SourceKind` names; the port took it as a
/// plain `String` and wrote it. That matters more than a rejected request: the
/// sources upsert sets `kind = excluded.kind`, so one bad value does not just
/// store wrongly, it OVERWRITES a good kind on an existing source — and
/// `sources::active_window` grades liveness off that column.
#[tokio::test]
async fn a_kind_the_fleet_does_not_know_is_refused_rather_than_written() {
    let (dir, addr) = serve().await;
    let mut seg = segment(vec![turn("00:00", "00:05", "hello")]);
    seg.kind = "not-a-kind".to_owned();

    let (status, _) = post_json(&addr, "/sync/segments", wire(&seg)).await;

    assert_eq!(status, 400, "the Python refuses this kind with a 400");
    assert_eq!(
        stored_kind(dir.path()),
        None,
        "a refused push must not have written the source row"
    );
}

/// The other half of the same rule: the six kinds the fleet DOES name must still
/// pass. A check that refuses everything would satisfy the test above.
#[tokio::test]
async fn every_kind_the_fleet_names_is_still_accepted() {
    let (_dir, addr) = serve().await;
    for (i, kind) in [
        "coreaudio",
        "lavfi",
        "rtsp",
        "tcp_pcm",
        "upload",
        "discovered",
    ]
    .into_iter()
    .enumerate()
    {
        let mut seg = segment(vec![turn("00:00", "00:05", "hello")]);
        seg.source_id = format!("src{i}");
        seg.kind = kind.to_owned();
        let (status, body) = post_json(&addr, "/sync/segments", wire(&seg)).await;
        assert_eq!(status, 200, "kind {kind} was refused: {body}");
    }
}

/// The batch route's first direct test: many segments, one round trip.
#[tokio::test]
async fn a_batch_stores_many_segments_in_one_request() {
    let (dir, addr) = serve().await;
    let mut a = segment(vec![turn("00:00", "00:05", "first")]);
    a.start = "2026-09-09T10:00:00+00:00".to_owned();
    let mut b = segment(vec![turn("01:00", "01:05", "second")]);
    b.start = "2026-09-09T10:02:00+00:00".to_owned();
    b.end = "2026-09-09T10:03:00+00:00".to_owned();

    let (status, body) = post_json(
        &addr,
        "/sync/segments/batch",
        serde_json::json!({"segments": [wire(&a), wire(&b)]}),
    )
    .await;

    assert_eq!(status, 200, "{body}");
    let conn = recalld::work::open_write(dir.path()).expect("db");
    assert_eq!(
        visible(&conn),
        vec!["first".to_owned(), "second".to_owned()],
        "both segments in the batch must be stored"
    );
}

/// ⚠ Parity on the FAILURE shape, not just the success one. The Python ingests a
/// batch sequentially and lets an item failure fail the whole request — so the
/// items BEFORE the bad one are already written when the 400 lands. Validating
/// the whole batch up front would be tidier and would not match: the Mac's
/// retry is what makes either safe, and only one of them is what happens today.
#[tokio::test]
async fn a_bad_kind_in_a_batch_fails_it_after_the_earlier_items_are_written() {
    let (dir, addr) = serve().await;
    let good = segment(vec![turn("00:00", "00:05", "first")]);
    let mut bad = segment(vec![turn("01:00", "01:05", "second")]);
    bad.source_id = "other".to_owned();
    bad.start = "2026-09-09T10:02:00+00:00".to_owned();
    bad.end = "2026-09-09T10:03:00+00:00".to_owned();
    bad.kind = "not-a-kind".to_owned();

    let (status, _) = post_json(
        &addr,
        "/sync/segments/batch",
        serde_json::json!({"segments": [wire(&good), wire(&bad)]}),
    )
    .await;

    assert_eq!(status, 400, "one bad item fails the whole request");
    let conn = recalld::work::open_write(dir.path()).expect("db");
    assert_eq!(
        visible(&conn),
        vec!["first".to_owned()],
        "the item before the bad one was already committed, as in the Python"
    );
}

// --- RULE 6: a token is not a licence to write anywhere ----------------------

/// The Mac is authenticated, so this is not about who is calling — it is about
/// what a STOLEN token can reach. Re-homing makes the answer structural rather
/// than vigilant: only the basename survives, so a hostile path has no directory
/// left in it to point outside the archive with.
///
/// ⚠ Ported from Python's `test_a_pushed_path_can_never_escape_the_fleet_archive`,
/// which had no Rust counterpart while the Rust was the one serving the route.
/// `ingest_segment` does call `safe_component`, so the behaviour was there and
/// only the proof was missing — and #1500 will not delete a Python test whose
/// rule nothing else pins.
#[test]
fn a_hostile_path_cannot_escape_the_archive_because_only_its_basename_survives() {
    let (dir, mut conn) = store();
    let mut seg = segment(vec![turn("00:00", "00:10", "x")]);
    seg.path = "/etc/../../root/.ssh/authorized_keys".to_owned();

    let stored = ingest_segment(&mut conn, &seg, dir.path());

    // Either refused outright, or re-homed — never a write outside the archive.
    if stored.is_ok() {
        let path: String = conn
            .query_row("SELECT path FROM audio_segments", [], |r| r.get(0))
            .unwrap();
        assert!(
            std::path::Path::new(&path).starts_with(dir.path()),
            "escaped the archive: {path}"
        );
        assert!(!path.contains(".."), "a traversal survived: {path}");
    }
}

/// A basename of `..` is not a name at all. Refused outright rather than
/// sanitised into something plausible: a push the fleet cannot make sense of is
/// an error, and inventing a filename for it would store audio under a name the
/// Mac will never ask for again.
///
/// ⚠ Ported from Python's `test_a_filename_that_is_not_a_filename_is_refused`.
#[test]
fn a_filename_that_is_not_a_filename_is_refused_rather_than_repaired() {
    let (dir, mut conn) = store();
    let mut seg = segment(vec![turn("00:00", "00:10", "x")]);
    seg.path = "/archive/usb/..".to_owned();

    let stored = ingest_segment(&mut conn, &seg, dir.path());

    assert!(stored.is_err(), "a nameless push was accepted");
    let rows: i64 = conn
        .query_row("SELECT count(*) FROM audio_segments", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 0, "a refused push still wrote a row");
}

/// ⚠ **The sharp end of the same bug: a malformed push BLANKS a good path.**
/// The insert is an upsert on `(source_id, start_utc)` with
/// `DO UPDATE SET path = excluded.path`, so a second push of the same identity
/// carrying an unusable path replaces a working pointer with an empty string —
/// 200 OK, nothing logged, and the audio is unreachable from the row that is
/// supposed to find it.
#[test]
fn a_malformed_repush_cannot_blank_the_path_of_a_segment_already_stored() {
    let (dir, mut conn) = store();
    push(
        &mut conn,
        dir.path(),
        &segment(vec![turn("00:00", "00:10", "x")]),
    );
    let good: String = conn
        .query_row("SELECT path FROM audio_segments", [], |r| r.get(0))
        .unwrap();
    assert!(
        !good.is_empty(),
        "precondition: the first push stored a path"
    );

    // Same identity, unusable path.
    let mut bad = segment(vec![turn("00:00", "00:10", "x")]);
    bad.path = "/archive/usb/..".to_owned();
    let _ = ingest_segment(&mut conn, &bad, dir.path());

    let after: String = conn
        .query_row("SELECT path FROM audio_segments", [], |r| r.get(0))
        .unwrap();
    assert_eq!(after, good, "a malformed repush blanked a stored path");
}
