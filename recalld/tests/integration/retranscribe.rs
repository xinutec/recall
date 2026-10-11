//! Transcribing a clip again: a request queues its job again, the clip keeps
//! its result until the new one lands, and a request still waiting can be
//! taken back. Which clips lost speech to a loop is measured, not guessed.

use audiocore::job::Kind;
use recalld::retranscribe;
use rusqlite::Connection;

const NOW: &str = "2026-09-27T18:00:00+00:00";
const BLOCK: &str = "usb-20260906T094500.flac";
const BLOCK_START: &str = "2026-09-06T09:45:00+00:00";

const LOOP_OVER_SPEECH: &str = r#"{"ok": true, "result": {"language": "en", "segments": [
    {"start": 0.0, "end": 2.0, "text": " real words here"},
    {"start": 10.0, "end": 20.0, "text": " getting getting getting getting getting getting"}
]}}"#;

/// The ingest plane with one transcribed usb clip.
fn ingest(dir: &std::path::Path) -> Connection {
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
            (Kind::TranscribeSegment, BLOCK, NOW, LOOP_OVER_SPEECH),
        )
        .expect("job");
    ingest
}

/// The job's finish and result.
fn job(ingest: &Connection) -> (Option<String>, Option<String>) {
    ingest
        .query_row(
            "SELECT done_utc, result FROM jobs WHERE filename = ?1",
            [BLOCK],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("job")
}

#[test]
fn a_clip_with_no_finished_transcription_is_skipped_and_nothing_changes() {
    let dir = tempfile::tempdir().expect("tmp");
    let ingest = ingest(dir.path());
    ingest
        .execute(
            "UPDATE jobs SET done_utc = NULL, result = NULL, state = 'queued'",
            [],
        )
        .expect("still queued");

    let asked = retranscribe::request(
        &ingest,
        &[BLOCK.to_owned(), "nope-20260906T094500.flac".to_owned()],
    )
    .expect("request");

    assert!(asked.queued.is_empty());
    assert_eq!(asked.skipped.len(), 2);
}

#[test]
fn a_waiting_request_taken_back_is_done_again_with_its_result_and_a_finished_one_is_not() {
    let dir = tempfile::tempdir().expect("tmp");
    let ingest = ingest(dir.path());
    let now = crate::stamp(NOW);

    retranscribe::request(&ingest, &[BLOCK.to_owned()]).expect("request");
    let (done, result) = job(&ingest);
    assert_eq!(done, None, "queued again");
    assert_eq!(
        result.as_deref(),
        Some(LOOP_OVER_SPEECH),
        "the result stays until a new one replaces it"
    );

    assert!(retranscribe::cancel(&ingest, BLOCK, &now).expect("cancel"));
    assert!(job(&ingest).0.is_some(), "done again");
    assert!(
        !retranscribe::cancel(&ingest, BLOCK, &now).expect("again"),
        "nothing waiting to take back"
    );
}

#[test]
fn a_candidate_lost_speech_under_a_loop_and_a_loop_over_silence_is_not_one() {
    let dir = tempfile::tempdir().expect("tmp");
    let ingest = ingest(dir.path());
    let placed = |regions: &str| {
        ingest
            .execute(
                "INSERT OR REPLACE INTO segment_speech
                     (filename, source, speech_seconds, computed_utc, regions)
                 VALUES (?1, 'usb', 6.0, ?2, ?3)",
                (BLOCK, NOW, regions),
            )
            .expect("speech");
    };

    // Speech 12-16 s sits under the loop at 10-20 s: 4 s lost.
    placed("[[0.0,2.0],[12.0,16.0]]");
    let found = retranscribe::candidates(&ingest, 1.0).expect("candidates");
    assert_eq!(found.len(), 1);
    assert!((found[0].looped_speech_s - 4.0).abs() < 1e-9, "{found:?}");

    // The same loop over silence lost nothing.
    placed("[[0.0,2.0]]");
    assert!(
        retranscribe::candidates(&ingest, 1.0)
            .expect("candidates")
            .is_empty()
    );

    // Asked for and still waiting: not offered again.
    placed("[[0.0,2.0],[12.0,16.0]]");
    retranscribe::request(&ingest, &[BLOCK.to_owned()]).expect("request");
    assert!(
        retranscribe::candidates(&ingest, 1.0)
            .expect("candidates")
            .is_empty()
    );
}
