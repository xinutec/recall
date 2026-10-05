//! Fetching a result for a clip whose lines no stored transcription backs:
//! the result arrives, and through both passes the lines stay as they were.

use audiocore::job::Kind;
use rusqlite::Connection;

const NOW: &str = "2026-10-05T10:00:00+00:00";
const BLOCK: &str = "usb-20260906T094500.flac";
const BLOCK_START: &str = "2026-09-06T09:45:00+00:00";

const OLD: &str = r#"{"ok": true, "result": {"language": "en", "segments": [
    {"start": 0.0, "end": 2.0, "text": " old words here", "confidence": 0.9,
     "words": [{"start": 0.0, "end": 0.6, "text": " old", "probability": 0.9},
               {"start": 0.6, "end": 1.2, "text": " words", "probability": 0.9},
               {"start": 1.2, "end": 2.0, "text": " here", "probability": 0.9}]}
]}}"#;

const FRESH: &str = r#"{"ok": true, "result": {"language": "en", "segments": [
    {"start": 0.0, "end": 2.0, "text": " new words", "confidence": 0.9,
     "words": [{"start": 0.0, "end": 1.0, "text": " new", "probability": 0.9},
               {"start": 1.0, "end": 2.0, "text": " words", "probability": 0.9}]},
    {"start": 20.0, "end": 22.0, "text": " and more", "confidence": 0.9,
     "words": [{"start": 20.0, "end": 21.0, "text": " and", "probability": 0.9},
               {"start": 21.0, "end": 22.0, "text": " more", "probability": 0.9}]}
]}}"#;

const TWO_SPEAKERS: &str = r#"{"ok": true, "result": {"turns": [
    {"speaker": "SPEAKER_00", "start": 0.0, "end": 1.0},
    {"speaker": "SPEAKER_01", "start": 1.0, "end": 30.0}]}}"#;

/// A clip whose lines an older pipeline wrote, and whose job rows are gone.
fn archive(dir: &std::path::Path) -> (Connection, Connection) {
    let mut meaning = Connection::open(dir.join("recall.sqlite")).expect("meaning");
    recalld::meaning_schema::ensure(&meaning).expect("schema");
    let audio = dir.join("ingest/usb").join(BLOCK);
    std::fs::create_dir_all(audio.parent().expect("dir")).expect("mkdir");
    std::fs::write(&audio, b"x").expect("audio");
    meaning
        .execute(
            "INSERT INTO sources (id, name, kind) VALUES ('usb', 'usb', 'coreaudio')",
            [],
        )
        .expect("source");
    meaning
        .execute(
            "INSERT INTO audio_segments (id, source_id, path, start_utc, end_utc, sample_rate, channels)
             VALUES (1, 'usb', ?1, ?2, '2026-09-06T09:46:00+00:00', 16000, 1)",
            (audio.to_str().expect("utf-8"), BLOCK_START),
        )
        .expect("the clip");
    let ingest = recalld::store::open(dir).expect("ingest");
    ingest
        .execute(
            "INSERT INTO segments (source, filename, start_utc, bytes, sha256, received_utc)
             VALUES ('usb', ?1, ?2, 1, 'x', ?2)",
            (BLOCK, BLOCK_START),
        )
        .expect("segment");
    ingest
        .execute(
            "INSERT INTO jobs (kind, filename, state, created_utc, done_utc, result)
             VALUES (?1, ?2, 'done', ?3, ?3, ?4)",
            (Kind::TranscribeSegment, BLOCK, NOW, OLD),
        )
        .expect("job");
    let now = crate::stamp(NOW);
    recalld::turns::write_pass(&mut meaning, &ingest, &now, 10).expect("turns");
    ingest
        .execute("DELETE FROM jobs", [])
        .expect("history gone");
    (meaning, ingest)
}

/// Text, start, end, speaker, hidden reason.
type Shown = (String, String, String, Option<String>, Option<String>);

/// Everything about the clip's lines a person can see.
fn lines(meaning: &Connection) -> Vec<Shown> {
    let mut stmt = meaning
        .prepare(
            "SELECT text, start_utc, end_utc, speaker_label, hidden_reason
             FROM transcript_segments WHERE audio_segment_id = 1 ORDER BY id",
        )
        .expect("prepare");
    stmt.query_map([], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
    })
    .expect("query")
    .collect::<Result<_, _>>()
    .expect("rows")
}

#[test]
fn the_result_arrives_and_the_lines_stay_as_they_were() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut meaning, ingest) = archive(dir.path());
    let before = lines(&meaning);
    assert!(!before.is_empty(), "the older pipeline wrote lines");

    let looked = recalld::backfill::queue(&meaning, &ingest, dir.path(), false).expect("dry run");
    assert_eq!(looked.queued, 1);
    let jobs = |ingest: &Connection| -> i64 {
        ingest
            .query_row("SELECT count(*) FROM jobs", [], |r| r.get(0))
            .expect("count")
    };
    assert_eq!(jobs(&ingest), 0, "a dry run writes nothing");

    let done = recalld::backfill::queue(&meaning, &ingest, dir.path(), true).expect("apply");
    assert_eq!(done.queued, 1);

    // The runner answers both jobs.
    ingest
        .execute(
            "UPDATE jobs SET state = 'done', done_utc = ?1, result = ?2 WHERE kind = ?3",
            (NOW, FRESH, Kind::TranscribeSegment),
        )
        .expect("transcribed");
    recalld::queue::derive_jobs(&ingest, chrono::Utc::now()).expect("follow-on");
    let diarized = ingest
        .execute(
            "UPDATE jobs SET state = 'done', done_utc = ?1, result = ?2 WHERE kind = ?3",
            (NOW, TWO_SPEAKERS, Kind::DiarizeSegment),
        )
        .expect("diarized");
    assert_eq!(diarized, 1, "the diarization followed on its own");

    let now = crate::stamp(NOW);
    recalld::turns::write_pass(&mut meaning, &ingest, &now, 10).expect("turns");
    recalld::diarized::write_pass(&mut meaning, &ingest, &now, 10).expect("speakers");
    assert_eq!(lines(&meaning), before, "nothing shown changed");

    let clip = recalld::clips::by_filename(&ingest, BLOCK)
        .expect("lookup")
        .expect("clip");
    let facts = recalld::rendering::facts(&ingest, &clip).expect("facts");
    assert!(
        facts.heard.is_some() && facts.voices.is_some(),
        "render has both"
    );

    let again = recalld::backfill::queue(&meaning, &ingest, dir.path(), true).expect("again");
    assert_eq!(again.queued, 0, "a clip with a result is not queued again");
}
