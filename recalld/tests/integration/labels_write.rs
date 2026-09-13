//! The labelling writes: names a person typed, which no pass can re-derive.

use recalld::labels_write::{hide_correction, set_correction_speaker, set_turn_speaker};
use rusqlite::Connection;

fn db() -> Connection {
    let conn = Connection::open_in_memory().expect("open");
    conn.execute_batch(
        "CREATE TABLE transcript_segments (
            id INTEGER PRIMARY KEY AUTOINCREMENT, text TEXT NOT NULL,
            asr_model TEXT, speaker_label TEXT, provenance TEXT, superseded_by INTEGER);
         CREATE TABLE corrections (
            id INTEGER PRIMARY KEY AUTOINCREMENT, transcript_segment_id INTEGER,
            speaker TEXT, hidden_reason TEXT);
         CREATE TABLE speaker_embeddings (
            id INTEGER PRIMARY KEY AUTOINCREMENT, source_correction_id INTEGER);",
    )
    .expect("schema");
    conn
}

/// The shape the correction path leaves behind: an original turn, the human turn
/// that superseded it (found again by provenance), the corpus pair, and the
/// voiceprint enrolled from the clip.
fn corrected(conn: &Connection, original: i64, speaker: &str) -> i64 {
    conn.execute(
        "INSERT INTO transcript_segments (id, text, asr_model) VALUES (?1, 'old', 'whisper')",
        [original],
    )
    .expect("original");
    conn.execute(
        "INSERT INTO transcript_segments (text, asr_model, speaker_label, provenance)
         VALUES ('new', 'human', ?1, ?2)",
        (speaker, format!("human correction of #{original}")),
    )
    .expect("human turn");
    conn.execute(
        "INSERT INTO corrections (transcript_segment_id, speaker) VALUES (?1, ?2)",
        (original, speaker),
    )
    .expect("pair");
    let correction_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO speaker_embeddings (source_correction_id) VALUES (?1)",
        [correction_id],
    )
    .expect("voiceprint");
    correction_id
}

fn label_of(conn: &Connection, provenance: &str) -> Option<String> {
    conn.query_row(
        "SELECT speaker_label FROM transcript_segments WHERE provenance = ?1",
        [provenance],
        |r| r.get(0),
    )
    .expect("read back")
}

#[test]
fn setting_a_turn_speaker_touches_only_that_turn() {
    let conn = db();
    conn.execute(
        "INSERT INTO transcript_segments (id, text) VALUES (1, 'a'), (2, 'b')",
        [],
    )
    .expect("turns");

    set_turn_speaker(&conn, 1, Some("Dr Lee")).expect("set");

    let first: Option<String> = conn
        .query_row(
            "SELECT speaker_label FROM transcript_segments WHERE id=1",
            [],
            |r| r.get(0),
        )
        .expect("read");
    let second: Option<String> = conn
        .query_row(
            "SELECT speaker_label FROM transcript_segments WHERE id=2",
            [],
            |r| r.get(0),
        )
        .expect("read");
    assert_eq!(first.as_deref(), Some("Dr Lee"));
    assert_eq!(second, None);
}

#[test]
fn clearing_a_turn_speaker_stores_null_not_an_empty_name() {
    let conn = db();
    conn.execute(
        "INSERT INTO transcript_segments (id, text, speaker_label) VALUES (1, 'a', 'Dr Lee')",
        [],
    )
    .expect("turn");

    set_turn_speaker(&conn, 1, None).expect("cleared");

    let label: Option<String> = conn
        .query_row(
            "SELECT speaker_label FROM transcript_segments WHERE id=1",
            [],
            |r| r.get(0),
        )
        .expect("read");
    assert_eq!(
        label, None,
        "a blank name would render as a speaker called nothing"
    );
}

#[test]
fn reassigning_a_correction_moves_the_pair_the_live_turn_and_the_voiceprint() {
    // ⚠ All three, or the timeline keeps showing the name that was just found to
    // be wrong while the corpus says otherwise.
    let mut conn = db();
    let correction = corrected(&conn, 41, "Alex");

    set_correction_speaker(&mut conn, correction, "Dr Lee").expect("reassigned");

    let pair: String = conn
        .query_row(
            "SELECT speaker FROM corrections WHERE id = ?1",
            [correction],
            |r| r.get(0),
        )
        .expect("pair");
    assert_eq!(pair, "Dr Lee");
    assert_eq!(
        label_of(&conn, "human correction of #41").as_deref(),
        Some("Dr Lee"),
        "the LIVE turn the timeline shows must move too"
    );
    let prints: i64 = conn
        .query_row("SELECT COUNT(*) FROM speaker_embeddings", [], |r| r.get(0))
        .expect("count");
    assert_eq!(prints, 0, "the clip must be re-enrolled under the new name");
}

#[test]
fn reassigning_does_not_touch_a_superseded_turn_carrying_the_same_provenance() {
    // A turn corrected twice leaves an older human turn with the same provenance.
    // Renaming the dead one would leave the live one under its old name.
    let mut conn = db();
    let correction = corrected(&conn, 41, "Alex");
    conn.execute(
        "INSERT INTO transcript_segments (text, asr_model, speaker_label, provenance, superseded_by)
         VALUES ('older', 'human', 'Alex', 'human correction of #41', 999)",
        [],
    )
    .expect("superseded human turn");

    set_correction_speaker(&mut conn, correction, "Dr Lee").expect("reassigned");

    let stale: String = conn
        .query_row(
            "SELECT speaker_label FROM transcript_segments WHERE superseded_by = 999",
            [],
            |r| r.get(0),
        )
        .expect("read");
    assert_eq!(stale, "Alex", "the dead version keeps what it had");
}

#[test]
fn reassigning_a_correction_with_no_live_turn_still_moves_the_pair() {
    // The pair can outlive its turn. Refusing here would leave a label nobody can
    // fix.
    let mut conn = db();
    conn.execute(
        "INSERT INTO corrections (transcript_segment_id, speaker) VALUES (NULL, 'Alex')",
        [],
    )
    .expect("orphan pair");
    let correction = conn.last_insert_rowid();

    set_correction_speaker(&mut conn, correction, "Dr Lee").expect("reassigned");

    let pair: String = conn
        .query_row(
            "SELECT speaker FROM corrections WHERE id = ?1",
            [correction],
            |r| r.get(0),
        )
        .expect("pair");
    assert_eq!(pair, "Dr Lee");
}

#[test]
fn hiding_a_correction_keeps_the_pair_and_drops_the_voiceprint() {
    // ⚠ Hidden, not deleted: that a person read this clip and judged it unusable
    // is itself evidence worth keeping.
    let mut conn = db();
    let correction = corrected(&conn, 41, "Alex");

    hide_correction(&mut conn, correction).expect("hidden");

    let (reason, speaker): (Option<String>, String) = conn
        .query_row(
            "SELECT hidden_reason, speaker FROM corrections WHERE id = ?1",
            [correction],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("pair");
    assert_eq!(reason.as_deref(), Some("review"));
    assert_eq!(speaker, "Alex", "the row survives");
    let prints: i64 = conn
        .query_row("SELECT COUNT(*) FROM speaker_embeddings", [], |r| r.get(0))
        .expect("count");
    assert_eq!(prints, 0, "but it stops matching");
}

#[test]
fn a_failed_reassignment_leaves_nothing_half_done() {
    // Drop the table the LAST statement needs: the pair and the live turn must
    // both roll back. If they did not, the corpus would say one name and the
    // timeline another, with nothing to show which was meant.
    let mut conn = db();
    let correction = corrected(&conn, 41, "Alex");
    conn.execute("DROP TABLE speaker_embeddings", [])
        .expect("drop");

    let failed = set_correction_speaker(&mut conn, correction, "Dr Lee");

    assert!(failed.is_err(), "the write must fail, not half-succeed");
    let pair: String = conn
        .query_row(
            "SELECT speaker FROM corrections WHERE id = ?1",
            [correction],
            |r| r.get(0),
        )
        .expect("pair");
    assert_eq!(pair, "Alex", "rolled back");
    assert_eq!(
        label_of(&conn, "human correction of #41").as_deref(),
        Some("Alex"),
        "and so did the live turn"
    );
}

// --- the correction itself ---------------------------------------------------

use recalld::labels_write::{CorrectError, Correction, apply_correction};

const NOW: &str = "2026-09-07T12:00:00+00:00";

fn correction_db() -> Connection {
    let conn = Connection::open_in_memory().expect("open");
    conn.execute_batch(
        "CREATE TABLE transcript_segments (
            id INTEGER PRIMARY KEY AUTOINCREMENT, audio_segment_id INTEGER,
            start_utc TEXT NOT NULL, end_utc TEXT NOT NULL, text TEXT NOT NULL,
            language TEXT, language_confidence REAL, asr_confidence REAL,
            asr_model TEXT, speaker_label TEXT, speaker_id INTEGER,
            speaker_cluster TEXT, provenance TEXT, created_utc TEXT,
            superseded_by INTEGER, hidden_reason TEXT, word_timings TEXT);
         CREATE VIRTUAL TABLE transcript_fts USING fts5(text, content='');
         CREATE TABLE corrections (
            id INTEGER PRIMARY KEY AUTOINCREMENT, transcript_segment_id INTEGER,
            audio_segment_id INTEGER, start_utc TEXT, end_utc TEXT,
            original_text TEXT, corrected_text TEXT, language TEXT,
            created_utc TEXT, speaker TEXT, audio_confidence REAL,
            hidden_reason TEXT);
         CREATE TABLE speaker_embeddings (
            id INTEGER PRIMARY KEY AUTOINCREMENT, source_correction_id INTEGER);",
    )
    .expect("schema");
    conn.execute(
        "INSERT INTO transcript_segments
             (id, audio_segment_id, start_utc, end_utc, text, language,
              language_confidence, asr_confidence, asr_model, speaker_cluster)
         VALUES (41, 7, '2026-07-03T09:51:00+00:00', '2026-07-03T09:51:04+00:00',
                 'mis heard words', 'en', 0.8, 0.42, 'mlx-whisper', 'SPEAKER_00')",
        [],
    )
    .expect("original turn");
    conn
}

#[test]
fn a_correction_supersedes_the_original_and_records_the_pair() {
    let mut conn = correction_db();

    let new_id = apply_correction(
        &mut conn,
        41,
        "  misheard words  ",
        NOW,
        &Correction::default(),
    )
    .expect("corrected");

    let (model, confidence, provenance, text): (String, f64, String, String) = conn
        .query_row(
            "SELECT asr_model, asr_confidence, provenance, text
             FROM transcript_segments WHERE id = ?1",
            [new_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .expect("new turn");
    assert_eq!(model, "human");
    assert!(
        (confidence - 1.0).abs() < f64::EPSILON,
        "a person read it; there is nothing to score"
    );
    assert_eq!(provenance, "human correction of #41");
    assert_eq!(text, "misheard words", "trimmed");

    let superseded: Option<i64> = conn
        .query_row(
            "SELECT superseded_by FROM transcript_segments WHERE id = 41",
            [],
            |r| r.get(0),
        )
        .expect("original");
    assert_eq!(superseded, Some(new_id));

    let (original, corrected): (String, String) = conn
        .query_row(
            "SELECT original_text, corrected_text FROM corrections WHERE transcript_segment_id = 41",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("pair");
    assert_eq!(original, "mis heard words");
    assert_eq!(corrected, "misheard words");
}

#[test]
fn a_corrected_turn_is_findable_by_search() {
    // ⚠ `transcript_fts` is a contentless FTS5 table maintained BY THE WRITER,
    // not by a trigger. Forgetting the insert breaks nothing loudly — it just
    // makes every human correction unsearchable, which is the likeliest way
    // anyone would go looking for one.
    let mut conn = correction_db();

    let new_id = apply_correction(
        &mut conn,
        41,
        "vorasidenib dosage",
        NOW,
        &Correction::default(),
    )
    .expect("corrected");

    let hit: i64 = conn
        .query_row(
            "SELECT rowid FROM transcript_fts WHERE transcript_fts MATCH 'vorasidenib'",
            [],
            |r| r.get(0),
        )
        .expect("the corrected text must be in the index");
    assert_eq!(hit, new_id);
}

#[test]
fn a_correction_carries_the_voice_forward_so_it_does_not_go_unknown() {
    let mut conn = correction_db();

    let new_id =
        apply_correction(&mut conn, 41, "fixed", NOW, &Correction::default()).expect("corrected");

    let cluster: Option<String> = conn
        .query_row(
            "SELECT speaker_cluster FROM transcript_segments WHERE id = ?1",
            [new_id],
            |r| r.get(0),
        )
        .expect("new turn");
    assert_eq!(cluster.as_deref(), Some("SPEAKER_00"));
}

#[test]
fn correcting_an_already_superseded_turn_is_refused() {
    // ⚠ A double-tap, or a second tab holding a stale id, would otherwise mint a
    // SECOND current human turn and a duplicate corpus pair — two "current"
    // versions of one moment, with nothing to say which is meant.
    let mut conn = correction_db();
    apply_correction(&mut conn, 41, "first", NOW, &Correction::default()).expect("first");

    let again = apply_correction(&mut conn, 41, "second", NOW, &Correction::default());

    assert!(
        matches!(again, Err(CorrectError::AlreadySuperseded(41))),
        "got {again:?}"
    );
    let humans: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM transcript_segments WHERE asr_model = 'human'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(humans, 1, "exactly one current human turn");
}

#[test]
fn a_blank_correction_is_refused_before_anything_is_written() {
    let mut conn = correction_db();

    let refused = apply_correction(&mut conn, 41, "   ", NOW, &Correction::default());

    assert!(
        matches!(refused, Err(CorrectError::Blank)),
        "got {refused:?}"
    );
    let pairs: i64 = conn
        .query_row("SELECT COUNT(*) FROM corrections", [], |r| r.get(0))
        .expect("count");
    assert_eq!(pairs, 0);
}

#[test]
fn correcting_a_turn_that_does_not_exist_names_the_id() {
    let mut conn = correction_db();

    let refused = apply_correction(&mut conn, 999, "text", NOW, &Correction::default());

    assert!(
        matches!(refused, Err(CorrectError::Missing(999))),
        "got {refused:?}"
    );
}

#[test]
fn an_overridden_span_and_language_reach_both_the_turn_and_the_pair() {
    // The boundary editor trims a clip to exactly one speaker, and a
    // mis-detected language is fixed in the same gesture.
    let mut conn = correction_db();

    let new_id = apply_correction(
        &mut conn,
        41,
        "gecorrigeerd",
        NOW,
        &Correction {
            speaker: Some("Dr Lee"),
            start: Some("2026-07-03T09:51:01+00:00"),
            end: Some("2026-07-03T09:51:03+00:00"),
            language: Some("nl"),
        },
    )
    .expect("corrected");

    let (start, end, lang, who): (String, String, String, String) = conn
        .query_row(
            "SELECT start_utc, end_utc, language, speaker_label
             FROM transcript_segments WHERE id = ?1",
            [new_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .expect("new turn");
    assert_eq!(start, "2026-07-03T09:51:01+00:00");
    assert_eq!(end, "2026-07-03T09:51:03+00:00");
    assert_eq!(lang, "nl");
    assert_eq!(who, "Dr Lee");

    let (pair_start, pair_lang): (String, String) = conn
        .query_row(
            "SELECT start_utc, language FROM corrections WHERE transcript_segment_id = 41",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("pair");
    assert_eq!(
        pair_start, "2026-07-03T09:51:01+00:00",
        "the pair records the TRIMMED span"
    );
    assert_eq!(pair_lang, "nl");
}

#[test]
fn the_pair_carries_the_original_audio_confidence_not_the_human_one() {
    // A readable label on faint audio is still good ASR data but too degraded to
    // enrol as a voice. Storing 1.0 here would lose the only signal that says so.
    let mut conn = correction_db();

    apply_correction(&mut conn, 41, "fixed", NOW, &Correction::default()).expect("corrected");

    let audio: f64 = conn
        .query_row("SELECT audio_confidence FROM corrections", [], |r| r.get(0))
        .expect("pair");
    assert!((audio - 0.42).abs() < f64::EPSILON, "got {audio}");
}

#[test]
fn a_failed_correction_leaves_no_orphan_turn_behind() {
    // ⚠ The Python committed after EACH of these steps, so a failure could leave
    // a human turn superseding nothing, or an original superseded with no pair to
    // show what it became. Drop the last table to prove the whole thing unwinds.
    let mut conn = correction_db();
    conn.execute("DROP TABLE corrections", []).expect("drop");

    let failed = apply_correction(&mut conn, 41, "fixed", NOW, &Correction::default());

    assert!(failed.is_err());
    let turns: i64 = conn
        .query_row("SELECT COUNT(*) FROM transcript_segments", [], |r| r.get(0))
        .expect("count");
    assert_eq!(turns, 1, "no half-written human turn");
    let superseded: Option<i64> = conn
        .query_row(
            "SELECT superseded_by FROM transcript_segments WHERE id = 41",
            [],
            |r| r.get(0),
        )
        .expect("original");
    assert_eq!(superseded, None, "and the original is untouched");
}

#[test]
fn an_overridden_span_is_respelled_the_way_every_stored_row_is() {
    // ⚠ These columns are compared and ordered as TEXT. A client sending `...Z`
    // where the table holds `...+00:00` writes a turn that sorts into the wrong
    // page — no error, just a turn that turns up in the wrong place.
    let mut conn = correction_db();

    let new_id = apply_correction(
        &mut conn,
        41,
        "fixed",
        NOW,
        &Correction {
            start: Some("2026-07-03T09:51:01Z"),
            end: Some("2026-07-03T09:51:03.5+00:00"),
            ..Correction::default()
        },
    )
    .expect("corrected");

    let (start, end): (String, String) = conn
        .query_row(
            "SELECT start_utc, end_utc FROM transcript_segments WHERE id = ?1",
            [new_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("new turn");
    assert_eq!(
        start, "2026-07-03T09:51:01+00:00",
        "Z becomes the sortable spelling"
    );
    assert_eq!(
        end, "2026-07-03T09:51:03.500000+00:00",
        "and a fraction is six digits, as datetime.isoformat writes it"
    );
}

#[test]
fn a_non_utc_offset_is_kept_rather_than_rebased() {
    // Tidier to convert, and wrong: the Python keeps the offset it was given, so
    // converting would write a text this table has never contained.
    let mut conn = correction_db();

    let new_id = apply_correction(
        &mut conn,
        41,
        "fixed",
        NOW,
        &Correction {
            start: Some("2026-07-03T10:51:01+01:00"),
            ..Correction::default()
        },
    )
    .expect("corrected");

    let start: String = conn
        .query_row(
            "SELECT start_utc FROM transcript_segments WHERE id = ?1",
            [new_id],
            |r| r.get(0),
        )
        .expect("new turn");
    assert_eq!(start, "2026-07-03T10:51:01+01:00");
}

#[test]
fn a_malformed_span_is_refused_rather_than_stored_verbatim() {
    let mut conn = correction_db();

    let refused = apply_correction(
        &mut conn,
        41,
        "fixed",
        NOW,
        &Correction {
            start: Some("last tuesday"),
            ..Correction::default()
        },
    );

    assert!(
        matches!(refused, Err(CorrectError::BadSpan)),
        "got {refused:?}"
    );
}
