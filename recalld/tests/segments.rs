//! The segment push — the one route whose failure destroys data rather than
//! reporting it. Every test here is paired with an ablation in the commit that
//! added it: a rule that can be removed with these still green is not covered.

use recalld::work::{SegmentIn, SegmentStoredOut, TurnIn, ingest_segment};
use rusqlite::Connection;

fn store() -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().expect("tmp");
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE sources (id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL, spec TEXT);
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
         CREATE TABLE deleted_segments (source_id TEXT NOT NULL, start_utc TEXT NOT NULL);",
    )
    .expect("schema");
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
    assert_eq!(name, "Renamed");
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
