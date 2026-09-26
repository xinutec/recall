//! The labelling writes: names a person typed, which no pass can re-derive.

use recalld::labels_write::{hide_correction, set_correction_speaker};
use rusqlite::Connection;

fn db() -> Connection {
    let conn = Connection::open_in_memory().expect("open");
    recalld::meaning_schema::ensure(&conn).expect("schema");
    conn
}

/// Every seeded row's span and creation time; these tests look at neither.
const AT: &str = "2026-07-03T09:51:00+00:00";

/// The shape the correction path leaves behind: an original turn, the human turn
/// that superseded it (found again by provenance), the corpus pair, and the
/// voiceprint enrolled from the clip.
fn corrected(conn: &Connection, original: i64, speaker: &str) -> i64 {
    conn.execute(
        "INSERT INTO transcript_segments (id, start_utc, end_utc, text, asr_model)
         VALUES (?1, ?2, ?2, 'old', 'whisper')",
        (original, AT),
    )
    .expect("original");
    conn.execute(
        "INSERT INTO transcript_segments
             (start_utc, end_utc, text, asr_model, speaker_label, provenance)
         VALUES (?3, ?3, 'new', 'human', ?1, ?2)",
        (speaker, format!("human correction of #{original}"), AT),
    )
    .expect("human turn");
    conn.execute(
        "INSERT INTO corrections
             (transcript_segment_id, start_utc, end_utc, original_text, corrected_text,
              created_utc, speaker)
         VALUES (?1, ?3, ?3, 'old', 'new', ?3, ?2)",
        (original, speaker, AT),
    )
    .expect("pair");
    let correction_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT OR IGNORE INTO speakers (name) VALUES (?1)",
        [speaker],
    )
    .expect("speaker");
    conn.execute(
        "INSERT INTO speaker_embeddings (speaker_id, vector, created_utc, source_correction_id)
         SELECT id, '[1.0]', ?2, ?1 FROM speakers WHERE name = ?3",
        (correction_id, AT, speaker),
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
fn reassigning_a_correction_moves_the_pair_the_live_turn_and_the_voiceprint() {
    // All three, or the timeline keeps showing the name just found to be wrong
    // while the corpus says otherwise.
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
        "INSERT INTO transcript_segments
             (start_utc, end_utc, text, asr_model, speaker_label, provenance, superseded_by)
         VALUES (?1, ?1, 'older', 'human', 'Alex', 'human correction of #41',
                 (SELECT id FROM transcript_segments WHERE text = 'new'))",
        [AT],
    )
    .expect("superseded human turn");

    set_correction_speaker(&mut conn, correction, "Dr Lee").expect("reassigned");

    let stale: String = conn
        .query_row(
            "SELECT speaker_label FROM transcript_segments WHERE text = 'older'",
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
        "INSERT INTO corrections
             (transcript_segment_id, start_utc, end_utc, original_text, corrected_text,
              created_utc, speaker)
         VALUES (NULL, ?1, ?1, 'old', 'new', ?1, 'Alex')",
        [AT],
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
    // Hidden, not deleted: a person judging this clip unusable is evidence worth
    // keeping.
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
    // Drop the table the last statement needs: the pair and the live turn must
    // both roll back, or the corpus and the timeline disagree.
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
    recalld::meaning_schema::ensure(&conn).expect("schema");
    conn.execute_batch(
        "INSERT INTO sources (id, name, kind) VALUES ('usb', 'usb', 'coreaudio');
         INSERT INTO audio_segments (id, source_id, path, start_utc, end_utc, sample_rate, channels)
         VALUES (7, 'usb', '/x.opus', '2026-07-03T09:51:00+00:00', '2026-07-03T09:52:00+00:00', 16000, 1);",
    )
    .expect("the clip");
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
        &crate::stamp(NOW),
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
    // ⚠ `transcript_fts` is a contentless FTS5 table maintained by the writer,
    // not a trigger. Forgetting the insert breaks nothing loudly; it makes every
    // human correction unsearchable.
    let mut conn = correction_db();

    let new_id = apply_correction(
        &mut conn,
        41,
        "vorasidenib dosage",
        &crate::stamp(NOW),
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

    let new_id = apply_correction(
        &mut conn,
        41,
        "fixed",
        &crate::stamp(NOW),
        &Correction::default(),
    )
    .expect("corrected");

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
    // A double-tap, or a second tab holding a stale id, must not mint a second
    // current human turn and a duplicate corpus pair.
    let mut conn = correction_db();
    apply_correction(
        &mut conn,
        41,
        "first",
        &crate::stamp(NOW),
        &Correction::default(),
    )
    .expect("first");

    let again = apply_correction(
        &mut conn,
        41,
        "second",
        &crate::stamp(NOW),
        &Correction::default(),
    );

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

    let refused = apply_correction(
        &mut conn,
        41,
        "   ",
        &crate::stamp(NOW),
        &Correction::default(),
    );

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

    let refused = apply_correction(
        &mut conn,
        999,
        "text",
        &crate::stamp(NOW),
        &Correction::default(),
    );

    assert!(
        matches!(refused, Err(CorrectError::Missing(999))),
        "got {refused:?}"
    );
}

#[test]
fn words_checked_is_recorded_even_when_the_text_is_unchanged() {
    let checked = |text: &str, words_checked: bool| {
        let mut conn = correction_db();
        apply_correction(
            &mut conn,
            41,
            text,
            &crate::stamp(NOW),
            &Correction {
                words_checked,
                ..Correction::default()
            },
        )
        .expect("corrected");
        conn.query_row(
            "SELECT words_checked FROM corrections WHERE transcript_segment_id = 41",
            [],
            |r| r.get::<_, Option<i64>>(0),
        )
        .expect("pair")
    };
    assert_eq!(checked("mis heard words", true), Some(1));
    assert_eq!(
        checked("mis heard words", false),
        None,
        "not said is NULL, never 0"
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
        &crate::stamp(NOW),
        &Correction {
            speaker: Some("Dr Lee"),
            start: Some("2026-07-03T09:51:01+00:00"),
            end: Some("2026-07-03T09:51:03+00:00"),
            language: Some("nl"),
            ..Correction::default()
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
    // enrol as a voice; storing 1.0 would lose the only signal that says so.
    let mut conn = correction_db();

    apply_correction(
        &mut conn,
        41,
        "fixed",
        &crate::stamp(NOW),
        &Correction::default(),
    )
    .expect("corrected");

    let audio: f64 = conn
        .query_row("SELECT audio_confidence FROM corrections", [], |r| r.get(0))
        .expect("pair");
    assert!((audio - 0.42).abs() < f64::EPSILON, "got {audio}");
}

#[test]
fn a_failed_correction_leaves_no_orphan_turn_behind() {
    // A partial correction would leave a human turn superseding nothing, or an
    // original superseded with no pair. Drop the last table to prove the whole
    // thing unwinds.
    let mut conn = correction_db();
    conn.execute("DROP TABLE corrections", []).expect("drop");

    let failed = apply_correction(
        &mut conn,
        41,
        "fixed",
        &crate::stamp(NOW),
        &Correction::default(),
    );

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
    // ⚠ These columns are compared and ordered as TEXT: a client's `...Z` where
    // the table holds `...+00:00` would sort the turn onto the wrong page,
    // silently.
    let mut conn = correction_db();

    let new_id = apply_correction(
        &mut conn,
        41,
        "fixed",
        &crate::stamp(NOW),
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
fn a_non_utc_offset_is_converted_to_the_stored_spelling() {
    // Compared as text, a `+01:00` stamp would sort an hour away from its moment.
    let mut conn = correction_db();

    let new_id = apply_correction(
        &mut conn,
        41,
        "fixed",
        &crate::stamp(NOW),
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
    assert_eq!(start, "2026-07-03T09:51:01+00:00");
}

#[test]
fn a_malformed_span_is_refused_rather_than_stored_verbatim() {
    let mut conn = correction_db();

    let refused = apply_correction(
        &mut conn,
        41,
        "fixed",
        &crate::stamp(NOW),
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

// ---- nobody spoke ----

use recalld::labels_write::{mark_no_speech, undo_no_speech};

fn invented(conn: &Connection) -> i64 {
    conn.execute(
        "INSERT INTO transcript_segments (start_utc, end_utc, text, asr_model, asr_confidence)
         VALUES ('2026-09-19T14:42:30+00:00', '2026-09-19T14:42:33+00:00',
                 'Thank you.', 'whisper', 0.58)",
        [],
    )
    .expect("turn");
    conn.last_insert_rowid()
}

fn now() -> audiocore::instant::Stamp {
    audiocore::instant::Stamp::parse(AT).expect("stamp")
}

#[test]
fn nobody_spoke_hides_the_turn_and_writes_no_new_one() {
    let mut conn = db();
    let id = invented(&conn);
    mark_no_speech(&mut conn, id, &now()).expect("mark");
    let (hidden, current): (Option<String>, i64) = conn
        .query_row(
            "SELECT (SELECT hidden_reason FROM transcript_segments WHERE id = ?1),
                    (SELECT COUNT(*) FROM transcript_segments
                      WHERE superseded_by IS NULL AND hidden_reason IS NULL)",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("read");
    assert_eq!(hidden.as_deref(), Some("nobody spoke"));
    assert_eq!(current, 0, "no turn stands in for the invented one");
}

#[test]
fn nobody_spoke_is_a_checked_pair_with_empty_text() {
    // The pair is an ASR label: this audio transcribes to nothing.
    let mut conn = db();
    let id = invented(&conn);
    mark_no_speech(&mut conn, id, &now()).expect("mark");
    let (original, corrected, checked): (String, String, Option<i64>) = conn
        .query_row(
            "SELECT original_text, corrected_text, words_checked FROM corrections
              WHERE transcript_segment_id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("pair");
    assert_eq!(
        (original.as_str(), corrected.as_str(), checked),
        ("Thank you.", "", Some(1))
    );
}

#[test]
fn nobody_spoke_protects_the_span_from_the_next_pass() {
    // Without this a re-transcription writes the same "Thank you." back.
    let mut conn = db();
    let id = invented(&conn);
    mark_no_speech(&mut conn, id, &now()).expect("mark");
    let at = |s: &str| s.parse::<chrono::DateTime<chrono::Utc>>().expect("t");
    let spans = recalld::turn_store::protected_between(
        &conn,
        at("2026-09-19T14:42:00Z"),
        at("2026-09-19T14:43:00Z"),
    )
    .expect("spans");
    assert_eq!(spans.len(), 1, "{spans:?}");
}

#[test]
fn nobody_spoke_twice_is_refused() {
    let mut conn = db();
    let id = invented(&conn);
    mark_no_speech(&mut conn, id, &now()).expect("first");
    assert!(matches!(
        mark_no_speech(&mut conn, id, &now()),
        Err(CorrectError::Hidden(i)) if i == id
    ));
    let pairs: i64 = conn
        .query_row("SELECT COUNT(*) FROM corrections", [], |r| r.get(0))
        .expect("count");
    assert_eq!(pairs, 1, "a double tap stores one pair");
}

#[test]
fn undoing_nobody_spoke_shows_the_turn_and_frees_the_span() {
    // A mis-tap must leave nothing behind: a pair left over would still keep
    // every later pass off the span.
    let mut conn = db();
    let id = invented(&conn);
    mark_no_speech(&mut conn, id, &now()).expect("mark");
    undo_no_speech(&mut conn, id).expect("undo");
    let (hidden, pairs): (Option<String>, i64) = conn
        .query_row(
            "SELECT (SELECT hidden_reason FROM transcript_segments WHERE id = ?1),
                    (SELECT COUNT(*) FROM corrections)",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("read");
    assert_eq!((hidden, pairs), (None, 0));
}

#[test]
fn only_nobody_spoke_can_be_undone() {
    // Undo must not show a turn hidden for any other reason.
    let mut conn = db();
    let id = invented(&conn);
    conn.execute(
        "UPDATE transcript_segments SET hidden_reason = 'silent minute' WHERE id = ?1",
        [id],
    )
    .expect("hide");
    assert!(matches!(
        undo_no_speech(&mut conn, id),
        Err(CorrectError::NotNobodySpoke(i)) if i == id
    ));
}

// ---- taking back a check ----

use recalld::labels_write::undo_correction;

fn checked(conn: &mut Connection) -> i64 {
    apply_correction(
        conn,
        41,
        "mis heard words",
        &crate::stamp(NOW),
        &Correction {
            words_checked: true,
            ..Correction::default()
        },
    )
    .expect("checked")
}

#[test]
fn undoing_a_check_leaves_the_line_as_the_machine_wrote_it() {
    let mut conn = correction_db();
    let human = checked(&mut conn);
    conn.execute(
        "INSERT INTO speakers (id, name) VALUES (1, 'Sam');
         ",
        [],
    )
    .expect("speaker");
    conn.execute(
        "INSERT INTO speaker_embeddings (speaker_id, vector, created_utc, source_segment_id)
         VALUES (1, '[1.0]', ?1, ?2)",
        (NOW, human),
    )
    .expect("a voiceprint enrolled from the checked turn");

    undo_correction(&mut conn, 41).expect("undone");

    let (superseded, humans, pairs, prints): (Option<i64>, i64, i64, i64) = conn
        .query_row(
            "SELECT (SELECT superseded_by FROM transcript_segments WHERE id = 41),
                    (SELECT COUNT(*) FROM transcript_segments WHERE asr_model = 'human'),
                    (SELECT COUNT(*) FROM corrections),
                    (SELECT COUNT(*) FROM speaker_embeddings)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .expect("read");
    assert_eq!((superseded, humans, pairs, prints), (None, 0, 0, 0));
}

#[test]
fn a_check_can_be_undone_once_and_a_line_never_checked_not_at_all() {
    let mut conn = correction_db();
    assert!(matches!(
        undo_correction(&mut conn, 41),
        Err(CorrectError::NotCorrected(41))
    ));
    checked(&mut conn);
    undo_correction(&mut conn, 41).expect("undone");
    assert!(matches!(
        undo_correction(&mut conn, 41),
        Err(CorrectError::NotCorrected(41))
    ));
}

#[test]
fn a_check_corrected_again_since_is_not_taken_back() {
    // Undo is for the mis-tap just made. A later edit replaced the checked
    // turn, and taking back the first would orphan it.
    let mut conn = correction_db();
    let human = checked(&mut conn);
    apply_correction(
        &mut conn,
        human,
        "misheard words",
        &crate::stamp(NOW),
        &Correction::default(),
    )
    .expect("edited again");
    assert!(matches!(
        undo_correction(&mut conn, 41),
        Err(CorrectError::NotCorrected(41))
    ));
}

#[test]
fn a_turn_is_words_checked_when_confirmed_or_retyped_not_when_renamed() {
    let words_checked = |conn: &Connection, id: i64| -> Option<i64> {
        conn.query_row(
            "SELECT words_checked FROM transcript_segments WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .expect("turn")
    };
    let mut conn = correction_db();
    let renamed = apply_correction(
        &mut conn,
        41,
        "mis heard words",
        &crate::stamp(NOW),
        &Correction {
            speaker: Some("Dr. 1"),
            ..Correction::default()
        },
    )
    .expect("renamed");
    assert_eq!(
        words_checked(&conn, renamed),
        None,
        "the machine's words, renamed"
    );

    let confirmed = apply_correction(
        &mut conn,
        renamed,
        "mis heard words",
        &crate::stamp(NOW),
        &Correction {
            words_checked: true,
            ..Correction::default()
        },
    )
    .expect("confirmed");
    assert_eq!(words_checked(&conn, confirmed), Some(1));

    let retyped = apply_correction(
        &mut conn,
        confirmed,
        "misheard words",
        &crate::stamp(NOW),
        &Correction::default(),
    )
    .expect("retyped");
    assert_eq!(
        words_checked(&conn, retyped),
        Some(1),
        "typed words are vouched for"
    );
}
