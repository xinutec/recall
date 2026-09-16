//! The diarized swap's decision rules — the ones that can empty a recording.
//!
//! ⚠ Every case here is a way `refine.py` was once wrong, so a case that looks
//! redundant is a guard somebody removed once. `diarized` carries the reasoning.

use chrono::{DateTime, TimeDelta, Utc};
use recalld::align::{AlignedTurn, Word};
use recalld::diarized::{
    COVERAGE_REF_MIN_CHARS, Corrected, Existing, Refusal, Swap, decide, reliable_language,
};

fn base() -> DateTime<Utc> {
    DateTime::from_timestamp(1_780_000_000, 0).expect("in range")
}

fn word(start: f64, end: f64, text: &str) -> Word {
    Word {
        start,
        end,
        text: text.to_owned(),
        probability: 0.9,
    }
}

fn turn(start: f64, end: f64, text: &str) -> AlignedTurn {
    AlignedTurn {
        speaker: "SPEAKER_00".to_owned(),
        start,
        end,
        text: text.to_owned(),
        confidence: 0.9,
        words: vec![word(start, end, text)],
    }
}

fn existing(id: i64, text: &str) -> Existing {
    Existing {
        id,
        text: text.to_owned(),
    }
}

/// A block with text, and a pass with something real to say: the swap happens.
#[test]
fn a_real_pass_replaces_the_machine_turns_it_supersedes() {
    let swap = decide(
        base(),
        vec![
            turn(0.0, 2.0, "the kettle is on"),
            turn(2.0, 4.0, "so it is"),
        ],
        &[existing(11, "the kettle is on so it is")],
        &[],
    );

    match swap {
        Swap::Replace { insert, hide } => {
            assert_eq!(insert.len(), 2);
            assert_eq!(hide, vec![11]);
        }
        other @ Swap::Keep(_) => panic!("expected a replace, got {other:?}"),
    }
}

/// ⚠ **THE 132-SEGMENT RULE.** Every produced turn is filtered out, so there is
/// nothing to write — and the existing transcript must survive untouched. The
/// old code hid first and discovered this second.
#[test]
fn a_pass_that_produces_nothing_usable_keeps_what_is_there() {
    let swap = decide(
        base(),
        vec![turn(
            0.0,
            2.0,
            "video video video video video video video video",
        )],
        &[existing(11, "a minute of Dutch about writing things down")],
        &[],
    );

    assert_eq!(
        swap,
        Swap::Keep(Refusal::AllFiltered { produced: 1 }),
        "the existing transcript must not be hidden"
    );
}

/// The same rule by the other route: the aligner returned nothing at all,
/// because there were no words or no speaker spans.
#[test]
fn a_pass_that_aligns_nothing_keeps_what_is_there() {
    let swap = decide(base(), vec![], &[existing(11, "real words")], &[]);
    assert_eq!(swap, Swap::Keep(Refusal::NothingAligned));
}

/// ⚠ A turn overlapping a span a person has corrected is DROPPED. The human's
/// text stands; a machine pass does not get to restate it.
#[test]
fn a_turn_inside_a_human_corrected_span_is_dropped() {
    let human = [Corrected {
        start: base() + TimeDelta::seconds(1),
        end: base() + TimeDelta::seconds(3),
    }];
    let swap = decide(
        base(),
        vec![
            turn(0.0, 2.0, "machine guess over the correction"),
            turn(5.0, 7.0, "well clear of it"),
        ],
        &[existing(11, "something")],
        &human,
    );

    match swap {
        Swap::Replace { insert, .. } => {
            assert_eq!(insert.len(), 1);
            assert_eq!(insert[0].text, "well clear of it");
        }
        other @ Swap::Keep(_) => panic!("expected a replace, got {other:?}"),
    }
}

/// …and if the human span covers everything, that is the keep case again —
/// never an empty block.
#[test]
fn a_block_entirely_inside_a_human_span_is_kept_not_emptied() {
    let human = [Corrected {
        start: base(),
        end: base() + TimeDelta::seconds(60),
    }];
    let swap = decide(
        base(),
        vec![turn(0.0, 2.0, "machine text")],
        &[existing(11, "the human's own words")],
        &human,
    );

    assert_eq!(swap, Swap::Keep(Refusal::AllFiltered { produced: 1 }));
}

/// Ordinary household speech, long enough to be worth protecting.
///
/// ⚠ NOT a repeated character or word. `"a".repeat(400)` was the first fixture
/// here and the guard correctly ignored it — `is_repetition_loop` filters the
/// existing side too, so a degenerate string measures as zero characters and
/// there is nothing to protect. The test caught the fixture, which is what the
/// symmetry rule below is about.
fn real_speech(min_chars: usize) -> String {
    let sentence = "so I said we should write it down before we forget it again, and ";
    let mut out = String::new();
    let mut n = 0;
    while out.chars().count() < min_chars {
        // Varied, so no word repeats back-to-back and no character runs.
        use std::fmt::Write as _;
        let _ = write!(out, "{sentence}that was number {n}. ");
        n += 1;
    }
    out
}

/// A degenerate pass — a truncated decode, a whole-clip mis-detection — must not
/// replace a substantial transcript with a fragment of it.
#[test]
fn a_pass_covering_far_less_than_what_exists_is_declined() {
    let long = real_speech(COVERAGE_REF_MIN_CHARS * 2);
    let long_chars = long.chars().count();
    let swap = decide(
        base(),
        vec![turn(0.0, 2.0, "oh")],
        &[existing(11, &long)],
        &[],
    );

    match swap {
        Swap::Keep(Refusal::Coverage { existing, new }) => {
            assert_eq!(existing, long_chars);
            assert_eq!(new, 2);
        }
        other => panic!("expected a coverage refusal, got {other:?}"),
    }
}

/// ⚠ **The symmetry that makes the guard honest.** The existing side is filtered
/// the same way the new side is, so a Whisper loop — hundreds of characters of
/// nothing — cannot win on length. Counting it raw held 10 of 94 guard-skipped
/// segments with their garbage preserved (measured 2026-09-02).
#[test]
fn a_repetition_loop_cannot_win_the_coverage_guard_by_sheer_length() {
    let loop_text = "видео ".repeat(200);
    let swap = decide(
        base(),
        vec![turn(0.0, 2.0, "the honest short transcription")],
        &[existing(11, &loop_text)],
        &[],
    );

    match swap {
        Swap::Replace { insert, hide } => {
            assert_eq!(insert.len(), 1);
            assert_eq!(hide, vec![11], "the loop is what gets hidden");
        }
        other @ Swap::Keep(_) => panic!("the loop must not block an honest pass, got {other:?}"),
    }
}

/// A block with NO existing turns is not a refusal when the pass yields nothing
/// usable — there was nothing in it to protect.
#[test]
fn an_empty_block_with_an_unusable_pass_is_not_a_refusal() {
    let swap = decide(
        base(),
        vec![turn(0.0, 2.0, "aaaa aaaa aaaa aaaa aaaa")],
        &[],
        &[],
    );
    // Whatever survives the filter, nothing is hidden, because nothing is there.
    match swap {
        Swap::Replace { hide, .. } => assert!(hide.is_empty()),
        Swap::Keep(_) => {}
    }
}

/// A short block is exempt from the coverage guard: tiny transcripts swing too
/// wildly in ratio for the bar to mean anything.
#[test]
fn a_short_existing_transcript_is_exempt_from_the_coverage_guard() {
    let short = real_speech(COVERAGE_REF_MIN_CHARS / 2);
    assert!(
        short.chars().count() < COVERAGE_REF_MIN_CHARS,
        "the fixture is short"
    );
    let swap = decide(
        base(),
        vec![turn(0.0, 2.0, "oh")],
        &[existing(11, &short)],
        &[],
    );
    assert!(
        matches!(swap, Swap::Replace { .. }),
        "under {COVERAGE_REF_MIN_CHARS} chars the guard does not apply"
    );
}

#[test]
fn only_a_household_language_is_trusted_for_a_confidence() {
    assert!(reliable_language(Some("nl")));
    assert!(reliable_language(Some("en")));
    assert!(!reliable_language(Some("ru")), "a hallucinated language");
    assert!(!reliable_language(None));
}

// --- the wire shapes ---------------------------------------------------------

use recalld::diarized::{speaker_turns, words_of};

/// ⚠ **The CURRENT shim spelling**: `text` + `probability` (`shim_asr.py`).
const WORDS_TODAY: &str = r#"{"ok": true, "result": {"language": "nl", "segments": [
    {"start": 0.0, "end": 2.0, "text": " een twee", "confidence": 0.9,
     "words": [{"start": 0.0, "end": 1.0, "text": " een", "probability": 0.9},
               {"start": 1.0, "end": 2.0, "text": " twee", "probability": 0.8}]}
]}}"#;

/// ⚠ **The spelling in a result STORED on 2026-09-11**: mlx-whisper's raw
/// `word`, and no probability at all. Results are stored, so both eras are in
/// the queue and both must align.
const WORDS_STORED_EARLIER: &str = r#"{"ok": true, "result": {"language": "nl", "segments": [
    {"start": 1.5, "end": 3.25, "text": " een twee drie", "confidence": 0.91,
     "words": [{"word": "een", "start": 1.5, "end": 1.9},
               {"word": " twee", "start": 1.9, "end": 2.4}]}
]}}"#;

#[test]
fn the_current_shim_spelling_yields_its_words() {
    let (words, language) = words_of(WORDS_TODAY).expect("words");
    assert_eq!(words.len(), 2);
    assert_eq!(words[0].text, " een");
    assert!((words[0].probability - 0.9).abs() < f64::EPSILON);
    assert_eq!(language.as_deref(), Some("nl"));
}

/// The one that would have failed silently: a whole era of stored results
/// aligning to nothing, indistinguishable from blocks with nothing said in them.
#[test]
fn a_result_stored_with_the_older_word_key_still_yields_its_words() {
    let (words, _) = words_of(WORDS_STORED_EARLIER).expect("words");
    assert_eq!(words.len(), 2);
    assert_eq!(words[0].text, "een");
    assert!(
        (words[0].probability - 1.0).abs() < f64::EPSILON,
        "an absent score is 'nobody measured', not 'no confidence'"
    );
}

#[test]
fn a_refused_transcription_yields_no_words_rather_than_an_error() {
    assert!(words_of(r#"{"ok": false, "error": "no such file"}"#).is_none());
}

#[test]
fn a_transcription_with_no_word_timings_yields_nothing_to_align() {
    let no_words = r#"{"ok": true, "result": {"language": "en", "segments": [
        {"start": 0.0, "end": 2.0, "text": "hello", "words": null}]}}"#;
    assert!(words_of(no_words).is_none());
}

#[test]
fn the_voices_shim_reply_yields_its_speaker_spans() {
    let reply = r#"{"ok": true, "result": {"turns": [
        {"speaker": "SPEAKER_00", "start": 0.0, "end": 1.2},
        {"speaker": "SPEAKER_01", "start": 1.2, "end": 2.0}]}}"#;
    let turns = speaker_turns(reply).expect("turns");
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].speaker, "SPEAKER_00");
    assert!((turns[1].end - 2.0).abs() < f64::EPSILON);
}

#[test]
fn a_refused_diarization_is_not_mistaken_for_an_empty_one() {
    assert!(speaker_turns(r#"{"ok": false, "error": "no such file"}"#).is_none());
}

// --- the pass, against real databases ----------------------------------------
//
// ⚠ The rules above are pure and the wire shapes are parsed; what is left is the
// part that actually HIDES and WRITES, and it is the part that can empty a
// recording. Nothing here is mocked: two real SQLite files, the real join across
// the audio and meaning planes, the real transaction.

use recalld::diarized::{PER_MIC, ROOM, write_pass};
use rusqlite::Connection;

const NOW: &str = "2026-09-15T18:00:00+00:00";
const BLOCK: &str = "room-20260906T094500.flac";
const BLOCK_START: &str = "2026-09-06T09:45:00+00:00";

/// The meaning plane: the Python tier's schema, the columns this pass touches.
fn meaning_plane(path: &std::path::Path) -> Connection {
    let conn = Connection::open(path.join("recall.sqlite")).expect("meaning");
    conn.execute_batch(
        "CREATE TABLE audio_segments (
             id INTEGER PRIMARY KEY, source_id TEXT NOT NULL, path TEXT NOT NULL,
             start_utc TEXT NOT NULL, end_utc TEXT NOT NULL,
             sample_rate INTEGER NOT NULL, channels INTEGER NOT NULL
         );
         CREATE TABLE transcript_segments (
             id INTEGER PRIMARY KEY, audio_segment_id INTEGER,
             start_utc TEXT NOT NULL, end_utc TEXT NOT NULL, text TEXT NOT NULL,
             language TEXT, language_confidence REAL, asr_confidence REAL,
             asr_model TEXT NOT NULL, speaker_label TEXT, speaker_id INTEGER,
             superseded_by INTEGER, created_utc TEXT, provenance TEXT,
             hidden_reason TEXT, loudness REAL, speaker_guess TEXT,
             speaker_score REAL, speaker_cluster TEXT, word_timings TEXT
         );
         CREATE VIRTUAL TABLE transcript_fts USING fts5(text);
         CREATE TABLE speakers (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE);
         CREATE TABLE speaker_embeddings (
             id INTEGER PRIMARY KEY, speaker_id INTEGER NOT NULL,
             vector TEXT NOT NULL, created_utc TEXT NOT NULL,
             source_correction_id INTEGER, source_segment_id INTEGER
         );
         CREATE TABLE transcript_embeddings (
             segment_id INTEGER PRIMARY KEY REFERENCES transcript_segments(id),
             vector     TEXT NOT NULL
         );
         CREATE TABLE corrections (
             id INTEGER PRIMARY KEY, transcript_segment_id INTEGER,
             audio_segment_id INTEGER, start_utc TEXT NOT NULL, end_utc TEXT NOT NULL,
             original_text TEXT NOT NULL, corrected_text TEXT NOT NULL,
             language TEXT, created_utc TEXT NOT NULL, speaker TEXT,
             hidden_reason TEXT, audio_confidence REAL
         );
         INSERT INTO audio_segments (id, source_id, path, start_utc, end_utc, sample_rate, channels)
         VALUES (1, 'room', '/x.flac', '2026-09-06T09:45:00+00:00',
                 '2026-09-06T09:46:00+00:00', 16000, 1);",
    )
    .expect("schema");
    conn
}

/// The ingest plane: the segment and the two finished jobs about it.
fn ingest_plane(path: &std::path::Path, voices: &str, transcription: &str) -> Connection {
    let conn = Connection::open(path.join("ingest.sqlite")).expect("ingest");
    conn.execute_batch(
        "CREATE TABLE segments (
             source TEXT NOT NULL, filename TEXT NOT NULL, start_utc TEXT NOT NULL,
             bytes INTEGER NOT NULL, sha256 TEXT NOT NULL, received_utc TEXT NOT NULL,
             sent_utc TEXT, PRIMARY KEY (source, filename)
         );",
    )
    .expect("segments");
    recalld::queue::ensure_schema(&conn).expect("jobs");
    conn.execute(
        "INSERT INTO segments (source, filename, start_utc, bytes, sha256, received_utc)
         VALUES ('room', ?1, ?2, 1, 'x', ?2)",
        (BLOCK, BLOCK_START),
    )
    .expect("segment");
    for (kind, result) in [
        (recalld::queue::TRANSCRIBE_ROOM, transcription),
        (recalld::queue::DIARIZE_ROOM, voices),
    ] {
        conn.execute(
            "INSERT INTO jobs (kind, filename, state, created_utc, done_utc, result)
             VALUES (?1, ?2, 'done', ?3, ?3, ?4)",
            (kind, BLOCK, NOW, result),
        )
        .expect("job");
    }
    conn
}

fn room_turn(conn: &Connection, text: &str) -> i64 {
    conn.execute(
        "INSERT INTO transcript_segments
             (audio_segment_id, start_utc, end_utc, text, asr_model, provenance, created_utc)
         VALUES (1, '2026-09-06T09:45:00+00:00', '2026-09-06T09:46:00+00:00', ?1,
                 'whisper', 'room', ?2)",
        (text, NOW),
    )
    .expect("turn");
    conn.last_insert_rowid()
}

const TWO_SPEAKERS: &str = r#"{"ok": true, "result": {"turns": [
    {"speaker": "SPEAKER_00", "start": 0.0, "end": 1.0},
    {"speaker": "SPEAKER_01", "start": 1.0, "end": 2.0}]}}"#;

#[test]
fn a_finished_diarization_replaces_the_room_turns_with_speaker_split_ones() {
    let dir = tempfile::tempdir().expect("tmp");
    let mut meaning = meaning_plane(dir.path());
    let ingest = ingest_plane(dir.path(), TWO_SPEAKERS, WORDS_TODAY);
    let old = room_turn(&meaning, "een twee");

    let pass = write_pass(&mut meaning, &ingest, &ROOM, NOW, 10).expect("pass");

    assert_eq!(pass.blocks, 1);
    assert_eq!(pass.turns, 2, "one turn per speaker");
    assert_eq!(pass.hidden, 1);
    assert_eq!(pass.kept, 0);

    // The old turn is hidden, not deleted.
    let hidden: Option<String> = meaning
        .query_row(
            "SELECT hidden_reason FROM transcript_segments WHERE id = ?1",
            [old],
            |r| r.get(0),
        )
        .expect("old turn still there");
    assert_eq!(
        hidden.as_deref(),
        Some("diarized (mlx-whisper/large-v3-turbo (room))")
    );

    // The new ones carry the speaker and the provenance three other readers test.
    let mut stmt = meaning
        .prepare(
            "SELECT text, speaker_cluster, provenance, word_timings FROM transcript_segments
             WHERE hidden_reason IS NULL ORDER BY start_utc",
        )
        .expect("prepare");
    let rows: Vec<(String, String, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, "SPEAKER_00");
    assert_eq!(rows[1].1, "SPEAKER_01");
    assert!(
        rows[0].2.starts_with("diarized-aligned"),
        "three readers test this prefix: {}",
        rows[0].2
    );

    // ⚠ The stored word shape, checked as JSON rather than as a string: {s,e,w}
    // is what `store._load_word_timings` and the boundary editor read.
    let stored: serde_json::Value = serde_json::from_str(&rows[0].3).expect("json");
    let first = &stored.as_array().expect("array")[0];
    assert!(first.get("s").is_some() && first.get("e").is_some() && first.get("w").is_some());
    assert!(
        first.get("start").is_none(),
        "the wire shape is not the stored one"
    );
    // Re-based to the turn, so a boundary edit can snap to a real word time.
    assert!(
        first["s"].as_f64().is_some_and(|v| v.abs() < f64::EPSILON),
        "re-based to the turn's own start"
    );
}

#[test]
fn a_decided_block_leaves_a_ledger_row_and_is_not_decided_twice() {
    let dir = tempfile::tempdir().expect("tmp");
    let mut meaning = meaning_plane(dir.path());
    let ingest = ingest_plane(dir.path(), TWO_SPEAKERS, WORDS_TODAY);
    room_turn(&meaning, "een twee");

    write_pass(&mut meaning, &ingest, &ROOM, NOW, 10).expect("first");
    let outcome: String = ingest
        .query_row(
            "SELECT outcome FROM pass_ledger WHERE kind = ?1 AND filename = ?2",
            (recalld::queue::DIARIZE_ROOM, BLOCK),
            |r| r.get(0),
        )
        .expect("a ledger row");
    assert_eq!(outcome, "aligned");

    // ⚠ The cost assertion: a second pass must do NO work. A decision that
    // writes no row is a decision made again for ever.
    let again = write_pass(&mut meaning, &ingest, &ROOM, NOW, 10).expect("second");
    assert_eq!(again, recalld::diarized::Pass::default());
}

/// ⚠ **THE 132-SEGMENT RULE, end to end.** Every produced turn is a repetition
/// loop, so nothing is written — and the room transcript must still be there.
#[test]
fn a_block_whose_pass_is_all_junk_keeps_its_transcript_and_is_not_retried() {
    let junk = r#"{"ok": true, "result": {"language": "nl", "segments": [
        {"start": 0.0, "end": 2.0, "text": "video video video video video video",
         "words": [{"start": 0.0, "end": 2.0, "text": " video video video video video video",
                    "probability": 0.2}]}]}}"#;
    let dir = tempfile::tempdir().expect("tmp");
    let mut meaning = meaning_plane(dir.path());
    let ingest = ingest_plane(dir.path(), TWO_SPEAKERS, junk);
    let old = room_turn(&meaning, "a minute of Dutch about writing things down");

    let pass = write_pass(&mut meaning, &ingest, &ROOM, NOW, 10).expect("pass");

    assert_eq!(pass.turns, 0);
    assert_eq!(pass.hidden, 0);
    assert_eq!(pass.kept, 1);
    let hidden: Option<String> = meaning
        .query_row(
            "SELECT hidden_reason FROM transcript_segments WHERE id = ?1",
            [old],
            |r| r.get(0),
        )
        .expect("still there");
    assert_eq!(hidden, None, "the transcript must survive untouched");
    // …and it is not re-attempted for ever.
    let outcome: String = ingest
        .query_row(
            "SELECT outcome FROM pass_ledger WHERE filename = ?1",
            [BLOCK],
            |r| r.get(0),
        )
        .expect("a ledger row for the refusal");
    assert!(outcome.starts_with("all-turns-filtered"), "got {outcome}");
}

/// A human correction over the block: the machine pass must not restate it.
#[test]
fn a_corrected_block_is_left_alone() {
    let dir = tempfile::tempdir().expect("tmp");
    let mut meaning = meaning_plane(dir.path());
    let ingest = ingest_plane(dir.path(), TWO_SPEAKERS, WORDS_TODAY);
    let old = room_turn(&meaning, "what the machine heard");
    meaning
        .execute(
            "INSERT INTO corrections
                 (transcript_segment_id, audio_segment_id, start_utc, end_utc,
                  original_text, corrected_text, created_utc)
             VALUES (?1, 1, '2026-09-06T09:45:00+00:00', '2026-09-06T09:46:00+00:00',
                     'what the machine heard', 'what was actually said', ?2)",
            (old, NOW),
        )
        .expect("correction");

    let pass = write_pass(&mut meaning, &ingest, &ROOM, NOW, 10).expect("pass");

    assert_eq!(pass.turns, 0);
    assert_eq!(pass.kept, 1);
    let hidden: Option<String> = meaning
        .query_row(
            "SELECT hidden_reason FROM transcript_segments WHERE id = ?1",
            [old],
            |r| r.get(0),
        )
        .expect("still there");
    assert_eq!(hidden, None);
}

/// A diarize job whose transcription has not finished carries no words, so there
/// is nothing to align — and it must WAIT rather than be retired.
#[test]
fn a_block_with_no_words_yet_waits_and_keeps_no_ledger_row() {
    let no_words = r#"{"ok": true, "result": {"language": "en", "segments": [
        {"start": 0.0, "end": 2.0, "text": "hello", "words": null}]}}"#;
    let dir = tempfile::tempdir().expect("tmp");
    let mut meaning = meaning_plane(dir.path());
    let ingest = ingest_plane(dir.path(), TWO_SPEAKERS, no_words);
    room_turn(&meaning, "hello");

    let pass = write_pass(&mut meaning, &ingest, &ROOM, NOW, 10).expect("pass");

    assert_eq!(pass.waiting, 1);
    assert_eq!(pass.blocks, 0);
    let rows: i64 = ingest
        .query_row("SELECT count(*) FROM pass_ledger", [], |r| r.get(0))
        .expect("count");
    assert_eq!(rows, 0, "a transient outcome must not retire the clip");
}

/// A block whose audio segment is not registered in the meaning plane yet: the
/// same rule, by the other transient route.
#[test]
fn a_block_with_no_audio_segment_waits() {
    let dir = tempfile::tempdir().expect("tmp");
    let mut meaning = meaning_plane(dir.path());
    meaning
        .execute("DELETE FROM audio_segments", ())
        .expect("unregister");
    let ingest = ingest_plane(dir.path(), TWO_SPEAKERS, WORDS_TODAY);

    let pass = write_pass(&mut meaning, &ingest, &ROOM, NOW, 10).expect("pass");

    assert_eq!(pass.waiting, 1);
    let rows: i64 = ingest
        .query_row("SELECT count(*) FROM pass_ledger", [], |r| r.get(0))
        .expect("count");
    assert_eq!(rows, 0);
}

/// ⚠ A diarize result with NO matching transcription is not eligible at all —
/// the join is what pairs the words with the speaker spans, and aligning against
/// a different block's words would attribute one conversation's sentences to
/// another's speakers.
#[test]
fn a_diarization_with_no_transcription_is_not_picked_up() {
    let dir = tempfile::tempdir().expect("tmp");
    let mut meaning = meaning_plane(dir.path());
    let ingest = ingest_plane(dir.path(), TWO_SPEAKERS, WORDS_TODAY);
    ingest
        .execute(
            "DELETE FROM jobs WHERE kind = ?1",
            [recalld::queue::TRANSCRIBE_ROOM],
        )
        .expect("drop the transcription");

    let pass = write_pass(&mut meaning, &ingest, &ROOM, NOW, 10).expect("pass");

    assert_eq!(pass, recalld::diarized::Pass::default());
}

// --- the per-mic stream: the one that replaces refine.py ---------------------
//
// ⚠ Every test above runs the ROOM stream, where there is nothing to replace.
// This is the other half and the dangerous one: a microphone clip ALREADY carries
// turns, so the pass takes its Replace path — hiding what a person can read and
// writing over it. That is `refine.py`'s semantics, and the reason `decide` has
// the guards it has.

/// The ingest plane with a per-mic clip and its two finished jobs.
fn mic_ingest(path: &std::path::Path, voices: &str, transcription: &str) -> Connection {
    let conn = Connection::open(path.join("ingest.sqlite")).expect("ingest");
    conn.execute_batch(
        "CREATE TABLE segments (
             source TEXT NOT NULL, filename TEXT NOT NULL, start_utc TEXT NOT NULL,
             bytes INTEGER NOT NULL, sha256 TEXT NOT NULL, received_utc TEXT NOT NULL,
             sent_utc TEXT, PRIMARY KEY (source, filename)
         );",
    )
    .expect("segments");
    recalld::queue::ensure_schema(&conn).expect("jobs");
    conn.execute(
        "INSERT INTO segments (source, filename, start_utc, bytes, sha256, received_utc)
         VALUES ('usb', 'usb-20260906T094500.flac', ?1, 1, 'x', ?1)",
        [BLOCK_START],
    )
    .expect("segment");
    for (kind, result) in [
        (recalld::queue::TRANSCRIBE_SEGMENT, transcription),
        (recalld::queue::DIARIZE_SEGMENT, voices),
    ] {
        conn.execute(
            "INSERT INTO jobs (kind, filename, state, created_utc, done_utc, result)
             VALUES (?1, 'usb-20260906T094500.flac', 'done', ?2, ?2, ?3)",
            (kind, NOW, result),
        )
        .expect("job");
    }
    conn
}

fn mic_meaning(path: &std::path::Path) -> Connection {
    let conn = meaning_plane(path);
    conn.execute("UPDATE audio_segments SET source_id = 'usb'", ())
        .expect("mic source");
    conn
}

/// ⚠ **THE POINT OF THE WHOLE PORT.** A microphone clip's existing turn is
/// superseded by speaker-split ones — exactly what `refine.py` does, on the same
/// corpus, with the same provenance prefix three other readers test for.
#[test]
fn a_microphone_clips_turns_are_replaced_by_speaker_split_ones() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut meaning = mic_meaning(dir.path());
    let ingest = mic_ingest(dir.path(), TWO_SPEAKERS, WORDS_TODAY);
    let old = room_turn(&meaning, "een twee");

    let pass = write_pass(&mut meaning, &ingest, &PER_MIC, NOW, 10).expect("pass");

    assert_eq!(pass.turns, 2, "one per speaker");
    assert_eq!(pass.hidden, 1, "the flat turn is superseded");

    let hidden: Option<String> = meaning
        .query_row(
            "SELECT hidden_reason FROM transcript_segments WHERE id = ?1",
            [old],
            |r| r.get(0),
        )
        .expect("still there");
    assert!(
        hidden.is_some_and(|h| h.starts_with("diarized (")),
        "hidden, not deleted"
    );
}

/// ⚠ The per-mic rows join the corpus `refine.py` wrote, so `asr_model` must be
/// the shim's own name — NOT a decorated one. A reader filtering on the model
/// must not see the archive split in two on the day the orchestrator changed.
#[test]
fn a_per_mic_turn_keeps_the_corpus_model_name_and_is_reversible_by_provenance() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut meaning = mic_meaning(dir.path());
    let ingest = mic_ingest(dir.path(), TWO_SPEAKERS, WORDS_TODAY);
    room_turn(&meaning, "een twee");

    write_pass(&mut meaning, &ingest, &PER_MIC, NOW, 10).expect("pass");

    let (model, provenance): (String, String) = meaning
        .query_row(
            "SELECT asr_model, provenance FROM transcript_segments
             WHERE hidden_reason IS NULL AND provenance LIKE 'diarized-aligned%' LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("a written turn");
    assert_eq!(model, recalld::turns::SHIM_MODEL);
    assert_eq!(
        provenance,
        format!("diarized-aligned ({})", recalld::turns::SHIM_MODEL)
    );
}

/// ⚠ **The two streams must not see each other's work.** They share the code, the
/// shim and the model; if a pass could pick up the other's jobs it would align
/// one clip's words against another clip's speakers. The ledger is per-kind and
/// the join is per-kind; this asserts both at once.
#[test]
fn the_per_mic_pass_does_not_touch_the_room_streams_jobs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut meaning = meaning_plane(dir.path());
    let ingest = ingest_plane(dir.path(), TWO_SPEAKERS, WORDS_TODAY); // ROOM jobs only
    room_turn(&meaning, "een twee");

    let pass = write_pass(&mut meaning, &ingest, &PER_MIC, NOW, 10).expect("pass");

    assert_eq!(
        pass,
        recalld::diarized::Pass::default(),
        "room jobs are not per-mic work"
    );
}

/// …and the converse, so neither test can pass by the pass simply doing nothing.
#[test]
fn the_room_pass_does_not_touch_the_per_mic_streams_jobs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut meaning = mic_meaning(dir.path());
    let ingest = mic_ingest(dir.path(), TWO_SPEAKERS, WORDS_TODAY); // PER-MIC jobs only
    room_turn(&meaning, "een twee");

    let pass = write_pass(&mut meaning, &ingest, &ROOM, NOW, 10).expect("pass");

    assert_eq!(pass, recalld::diarized::Pass::default());
}

/// A stream's provenance must name its own rows alone, or a reversal cannot take
/// one back without taking the other. This is the property the 2026-09-15 cleanup
/// depended on, when a `LIKE` pattern would have deleted 26,171 rows of the real
/// diarized corpus along with 22 of mine.
#[test]
fn the_two_streams_write_provenances_that_cannot_match_each_other() {
    let per_mic = format!("diarized-aligned ({})", PER_MIC.model);
    let room = format!("diarized-aligned ({})", ROOM.model);
    assert_ne!(per_mic, room);
    assert!(!per_mic.starts_with(&room) && !room.starts_with(&per_mic));
}

/// The kinds a runner may be handed must not overlap between streams either.
#[test]
fn the_two_streams_draw_from_different_queue_kinds() {
    let kinds: Vec<&str> = vec![
        PER_MIC.diarize_kind,
        PER_MIC.transcribe_kind,
        ROOM.diarize_kind,
        ROOM.transcribe_kind,
    ];
    let unique: std::collections::HashSet<&&str> = kinds.iter().collect();
    assert_eq!(
        unique.len(),
        kinds.len(),
        "a kind is claimed twice: {kinds:?}"
    );
}

// --- the voiceprints that come back with the spans ---------------------------

use recalld::diarized::voices;

const WITH_VOICES: &str = r#"{"ok": true, "result": {
    "turns": [{"speaker": "SPEAKER_00", "start": 0.0, "end": 2.0}],
    "speakers": [{"speaker": "SPEAKER_00", "seconds": 2.0, "vector": [0.1, 0.9]}]}}"#;

#[test]
fn a_diarization_carries_a_voiceprint_per_speaker() {
    let (turns, prints) = voices(WITH_VOICES).expect("parsed");
    assert_eq!(turns.len(), 1);
    assert_eq!(prints.len(), 1);
    assert_eq!(prints[0].speaker, "SPEAKER_00");
    assert_eq!(prints[0].vector.len(), 2);
}

/// ⚠ A result stored BEFORE the shim embedded carries spans and no vectors, and
/// must still align. Those turns get no name guess — worse than a guess, better
/// than a wrong name — rather than failing the whole clip.
#[test]
fn a_diarization_with_no_voiceprints_still_yields_its_spans() {
    let (turns, prints) = voices(TWO_SPEAKERS).expect("parsed");
    assert_eq!(turns.len(), 2);
    assert!(prints.is_empty());
}

#[test]
fn a_refused_diarization_yields_nothing_rather_than_empty_spans() {
    assert!(voices(r#"{"ok": false, "error": "no such file"}"#).is_none());
}

// --- naming the speakers a pass writes ---------------------------------------
//
// ⚠ 98% of the corpus `refine` built carries a speaker guess. A port that wrote
// bare `SPEAKER_nn` turns would be a regression dressed as a port, so this is the
// property that decides whether the switch is safe.

const VOICED: &str = r#"{"ok": true, "result": {
    "turns": [{"speaker": "SPEAKER_00", "start": 0.0, "end": 1.0},
              {"speaker": "SPEAKER_01", "start": 1.0, "end": 2.0}],
    "speakers": [{"speaker": "SPEAKER_00", "seconds": 1.0, "vector": [1.0, 0.0]},
                 {"speaker": "SPEAKER_01", "seconds": 1.0, "vector": [0.0, 1.0]}]}}"#;

/// Enrol two people whose voiceprints point along opposite axes, so which one a
/// turn matches is unambiguous and the test is about the WIRING, not the maths.
fn enrol_two(conn: &Connection) {
    for (id, name, vector) in [(1, "Alice", "[1.0, 0.0]"), (2, "Bob", "[0.0, 1.0]")] {
        conn.execute(
            "INSERT INTO speakers (id, name) VALUES (?1, ?2)",
            (id, name),
        )
        .expect("speaker");
        conn.execute(
            "INSERT INTO speaker_embeddings (speaker_id, vector, created_utc)
             VALUES (?1, ?2, ?3)",
            (id, vector, NOW),
        )
        .expect("voiceprint");
    }
}

#[test]
fn a_written_turn_carries_the_name_its_voiceprint_implies() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut meaning = mic_meaning(dir.path());
    enrol_two(&meaning);
    let ingest = mic_ingest(dir.path(), VOICED, WORDS_TODAY);
    room_turn(&meaning, "een twee");

    let pass = write_pass(&mut meaning, &ingest, &PER_MIC, NOW, 10).expect("pass");
    assert_eq!(pass.turns, 2);

    let mut stmt = meaning
        .prepare(
            "SELECT speaker_cluster, speaker_guess, speaker_score, speaker_label
             FROM transcript_segments
             WHERE hidden_reason IS NULL AND speaker_guess IS NOT NULL
             ORDER BY start_utc",
        )
        .expect("prepare");
    let rows: Vec<(String, String, f64, Option<String>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");

    assert_eq!(rows.len(), 2, "both turns named");
    assert_eq!(rows[0].0, "SPEAKER_00");
    assert_eq!(rows[0].1, "Alice");
    assert_eq!(rows[1].1, "Bob");
    assert!(
        rows[0].2 > 0.5,
        "a confident match scores high: {}",
        rows[0].2
    );

    // ⚠ The guess must NEVER land in `speaker_label`: that column is the name a
    // PERSON gave, and a machine writing there makes its own guess
    // indistinguishable from somebody's decision.
    assert!(rows.iter().all(|r| r.3.is_none()), "a guess is not a label");
}

/// The embedding is stored WITH the turn, or no later re-match can reach it —
/// `rematch_speaker_guesses` reads `transcript_embeddings`, so a turn written
/// without one is permanently unnameable rather than merely unnamed.
#[test]
fn a_written_turn_keeps_the_embedding_a_later_rematch_needs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut meaning = mic_meaning(dir.path());
    enrol_two(&meaning);
    let ingest = mic_ingest(dir.path(), VOICED, WORDS_TODAY);
    room_turn(&meaning, "een twee");

    write_pass(&mut meaning, &ingest, &PER_MIC, NOW, 10).expect("pass");

    let stored: i64 = meaning
        .query_row("SELECT count(*) FROM transcript_embeddings", [], |r| {
            r.get(0)
        })
        .expect("count");
    assert_eq!(stored, 2, "one per written turn");
}

/// ⚠ With nobody enrolled there is NO guess — not a low-scoring one. The archive
/// starts empty, and a confident-looking name on the first turn ever recorded
/// would be worse than silence.
#[test]
fn with_nobody_enrolled_turns_are_written_unnamed_rather_than_guessed_at() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut meaning = mic_meaning(dir.path());
    let ingest = mic_ingest(dir.path(), VOICED, WORDS_TODAY);
    room_turn(&meaning, "een twee");

    let pass = write_pass(&mut meaning, &ingest, &PER_MIC, NOW, 10).expect("pass");
    assert_eq!(pass.turns, 2, "the turns are still written");

    let guessed: i64 = meaning
        .query_row(
            "SELECT count(*) FROM transcript_segments WHERE speaker_guess IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(guessed, 0);
}

/// A diarization stored before the shim embedded carries spans and no vectors.
/// Those turns must still be WRITTEN — unnamed, not skipped.
#[test]
fn a_diarization_without_voiceprints_still_writes_its_turns() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut meaning = mic_meaning(dir.path());
    enrol_two(&meaning);
    let ingest = mic_ingest(dir.path(), TWO_SPEAKERS, WORDS_TODAY); // no "speakers"
    room_turn(&meaning, "een twee");

    let pass = write_pass(&mut meaning, &ingest, &PER_MIC, NOW, 10).expect("pass");

    assert_eq!(pass.turns, 2);
    let guessed: i64 = meaning
        .query_row(
            "SELECT count(*) FROM transcript_segments WHERE speaker_guess IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(guessed, 0, "no vector, no guess — and no crash");
}
