//! Transcribing a clip again, through the real passes: the old machine lines
//! give way, a person's stay, speakers come after the new lines, and it can be
//! taken back.

use audiocore::job::Kind;
use recalld::retranscribe::{self, Undone};
use rusqlite::Connection;

const NOW: &str = "2026-09-27T18:00:00+00:00";
const BLOCK: &str = "usb-20260906T094500.flac";
const BLOCK_START: &str = "2026-09-06T09:45:00+00:00";

const LOOPED: &str = r#"{"ok": true, "result": {"language": "en", "segments": [
    {"start": 0.0, "end": 2.0, "text": "old words here", "confidence": 0.9,
     "words": [{"start": 0.0, "end": 0.6, "text": " old", "probability": 0.9},
               {"start": 0.6, "end": 1.2, "text": " words", "probability": 0.9},
               {"start": 1.2, "end": 2.0, "text": " here", "probability": 0.9}]}
]}}"#;

const FRESH: &str = r#"{"ok": true, "result": {"language": "en", "segments": [
    {"start": 0.0, "end": 2.0, "text": " new words here", "confidence": 0.9,
     "words": [{"start": 0.0, "end": 0.6, "text": " new", "probability": 0.9},
               {"start": 0.6, "end": 1.2, "text": " words", "probability": 0.9},
               {"start": 1.2, "end": 2.0, "text": " here", "probability": 0.9}]},
    {"start": 20.0, "end": 22.0, "text": " and more of them", "confidence": 0.9,
     "words": [{"start": 20.0, "end": 20.5, "text": " and", "probability": 0.9},
               {"start": 20.5, "end": 21.0, "text": " more", "probability": 0.9},
               {"start": 21.0, "end": 21.5, "text": " of", "probability": 0.9},
               {"start": 21.5, "end": 22.0, "text": " them", "probability": 0.9}]}
]}}"#;

const ONE_SPEAKER: &str = r#"{"ok": true, "result": {"turns": [
    {"speaker": "SPEAKER_00", "start": 0.0, "end": 30.0}]}}"#;

fn planes(dir: &std::path::Path) -> (Connection, Connection) {
    let meaning = Connection::open(dir.join("recall.sqlite")).expect("meaning");
    recalld::meaning_schema::ensure(&meaning).expect("schema");
    meaning
        .execute_batch(
            "INSERT INTO sources (id, name, kind) VALUES ('usb', 'usb', 'coreaudio');
             INSERT INTO audio_segments (id, source_id, path, start_utc, end_utc, sample_rate, channels)
             VALUES (1, 'usb', '/x.flac', '2026-09-06T09:45:00+00:00',
                     '2026-09-06T09:46:00+00:00', 16000, 1);",
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
    for (kind, result) in [
        (Kind::TranscribeSegment, LOOPED),
        (Kind::DiarizeSegment, ONE_SPEAKER),
    ] {
        ingest
            .execute(
                "INSERT INTO jobs (kind, filename, state, created_utc, done_utc, result)
                 VALUES (?1, ?2, 'done', ?3, ?3, ?4)",
                (kind, BLOCK, NOW, result),
            )
            .expect("job");
    }
    (meaning, ingest)
}

fn passes(meaning: &mut Connection, ingest: &Connection) -> (usize, usize) {
    let now = crate::stamp(NOW);
    let turns = recalld::turns::write_pass(meaning, ingest, &now, 10).expect("turns");
    let speakers = recalld::diarized::write_pass(meaning, ingest, &now, 10).expect("speakers");
    (turns.blocks, speakers.blocks)
}

/// The clip's lines as (text, hidden reason), in time order.
fn lines(meaning: &Connection) -> Vec<(String, Option<String>)> {
    let mut stmt = meaning
        .prepare(
            "SELECT text, hidden_reason FROM transcript_segments
             WHERE audio_segment_id = 1 AND superseded_by IS NULL ORDER BY start_utc, id",
        )
        .expect("prepare");
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
}

fn shown(meaning: &Connection) -> Vec<String> {
    lines(meaning)
        .into_iter()
        .filter(|(_, hidden)| hidden.is_none())
        .map(|(text, _)| text)
        .collect()
}

fn rows(ingest: &Connection, sql: &str) -> i64 {
    ingest.query_row(sql, [BLOCK], |r| r.get(0)).expect("count")
}

/// The transcription lands, as the runner's `done` would leave it.
fn lands(ingest: &Connection, result: &str) {
    ingest
        .execute(
            "UPDATE jobs SET state = 'done', done_utc = ?1, result = ?2
             WHERE kind = ?3 AND filename = ?4",
            (NOW, result, Kind::TranscribeSegment, BLOCK),
        )
        .expect("done");
}

#[test]
fn new_lines_replace_the_machine_ones_a_persons_line_stays_and_speakers_come_after() {
    let dir = tempfile::tempdir().expect("tmp");
    let (mut meaning, ingest) = planes(dir.path());
    passes(&mut meaning, &ingest);
    assert_eq!(shown(&meaning), ["old words here"]);
    // A person's line, later in the minute: owned, so no pass may move it.
    meaning
        .execute(
            "INSERT INTO transcript_segments (audio_segment_id, start_utc, end_utc, text, asr_model)
             VALUES (1, '2026-09-06T09:45:40+00:00', '2026-09-06T09:45:42+00:00', 'mine', 'human')",
            [],
        )
        .expect("a person's line");

    let asked =
        retranscribe::request(&ingest, &[BLOCK.to_owned()], &crate::stamp(NOW)).expect("request");
    assert_eq!(asked.queued, [BLOCK]);
    assert_eq!(
        rows(
            &ingest,
            "SELECT count(*) FROM jobs WHERE filename = ?1 AND state = 'queued'"
        ),
        1,
        "back in Whisper's queue"
    );

    lands(&ingest, FRESH);
    // ⚠ The order this guards: the speaker pass must not run before the lines.
    let now = crate::stamp(NOW);
    let early = recalld::diarized::write_pass(&mut meaning, &ingest, &now, 10).expect("speakers");
    assert_eq!(early.blocks, 0, "the speaker pass waits for the new lines");

    let (turns, speakers) = passes(&mut meaning, &ingest);
    assert_eq!(
        (turns, speakers),
        (1, 1),
        "lines written, then speakers added"
    );
    let now_shown = shown(&meaning);
    assert!(now_shown.contains(&"mine".to_owned()), "{now_shown:?}");
    assert!(
        now_shown.iter().any(|t| t.contains("new words")),
        "{now_shown:?}"
    );
    assert!(
        !now_shown.iter().any(|t| t.contains("old words")),
        "{now_shown:?}"
    );
    assert!(
        lines(&meaning).contains(&(
            "old words here".to_owned(),
            Some("set aside for re-transcription".to_owned())
        )),
        "set aside, not deleted"
    );
    assert_eq!(
        rows(
            &ingest,
            "SELECT count(*) FROM retranscribe_requests WHERE filename = ?1"
        ),
        0
    );

    assert_eq!(
        retranscribe::undo(&mut meaning, &ingest, BLOCK).expect("undo"),
        Undone::Restored
    );
    let back = shown(&meaning);
    assert!(back.contains(&"old words here".to_owned()), "{back:?}");
    assert!(back.contains(&"mine".to_owned()), "{back:?}");
    assert!(!back.iter().any(|t| t.contains("new words")), "{back:?}");
}

#[test]
fn a_clip_with_no_finished_transcription_is_skipped_and_nothing_changes() {
    let dir = tempfile::tempdir().expect("tmp");
    let (_meaning, ingest) = planes(dir.path());
    ingest
        .execute(
            "UPDATE jobs SET done_utc = NULL, result = NULL, state = 'queued' WHERE kind = ?1",
            [Kind::TranscribeSegment],
        )
        .expect("still queued");
    let asked = retranscribe::request(
        &ingest,
        &[BLOCK.to_owned(), "nope-20260906T094500.flac".to_owned()],
        &crate::stamp(NOW),
    )
    .expect("request");
    assert!(asked.queued.is_empty());
    assert_eq!(asked.skipped.len(), 2);
    assert_eq!(
        rows(
            &ingest,
            "SELECT count(*) FROM retranscribe_requests WHERE filename = ?1"
        ),
        0
    );
}

#[test]
fn taking_back_a_request_still_waiting_leaves_the_old_lines_and_writes_nothing_new() {
    let dir = tempfile::tempdir().expect("tmp");
    let (mut meaning, ingest) = planes(dir.path());
    passes(&mut meaning, &ingest);
    retranscribe::request(&ingest, &[BLOCK.to_owned()], &crate::stamp(NOW)).expect("request");

    assert_eq!(
        retranscribe::undo(&mut meaning, &ingest, BLOCK).expect("undo"),
        Undone::Cancelled
    );
    lands(&ingest, FRESH);
    passes(&mut meaning, &ingest);
    assert_eq!(
        shown(&meaning),
        ["old words here"],
        "the late result is not written"
    );
}

#[test]
fn a_clip_never_transcribed_again_has_nothing_to_take_back() {
    let dir = tempfile::tempdir().expect("tmp");
    let (mut meaning, ingest) = planes(dir.path());
    passes(&mut meaning, &ingest);
    assert!(matches!(
        retranscribe::undo(&mut meaning, &ingest, BLOCK),
        Err(retranscribe::UndoError::NothingSetAside)
    ));
}

#[test]
fn a_clip_the_speaker_pass_never_decided_still_gets_its_lines_before_its_speakers() {
    // The case the request row itself must guard: no speaker ledger row to hold
    // the speaker pass off, so without the request it would write its own lines
    // onto the clip the moment the transcription lands, and the turns pass would
    // then skip the clip.
    let dir = tempfile::tempdir().expect("tmp");
    let (mut meaning, ingest) = planes(dir.path());
    let now = crate::stamp(NOW);
    recalld::turns::write_pass(&mut meaning, &ingest, &now, 10).expect("turns");
    retranscribe::request(&ingest, &[BLOCK.to_owned()], &now).expect("request");
    lands(&ingest, FRESH);

    let early = recalld::diarized::write_pass(&mut meaning, &ingest, &now, 10).expect("speakers");
    assert_eq!(early.blocks, 0, "the speaker pass waits for the new lines");
    let (turns, speakers) = passes(&mut meaning, &ingest);
    assert_eq!((turns, speakers), (1, 1));
    assert!(shown(&meaning).iter().any(|t| t.contains("more of them")));
}

const LOOP_OVER_SPEECH: &str = r#"{"ok": true, "result": {"language": "en", "segments": [
    {"start": 0.0, "end": 2.0, "text": " real words here"},
    {"start": 10.0, "end": 20.0, "text": " getting getting getting getting getting getting"}
]}}"#;

#[test]
fn a_candidate_lost_speech_under_a_loop_and_a_loop_over_silence_is_not_one() {
    let dir = tempfile::tempdir().expect("tmp");
    let (_meaning, ingest) = planes(dir.path());
    ingest
        .execute(
            "UPDATE jobs SET result = ?1 WHERE kind = ?2",
            (LOOP_OVER_SPEECH, Kind::TranscribeSegment),
        )
        .expect("a looped result");
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

    // Already asked for: not offered again.
    placed("[[0.0,2.0],[12.0,16.0]]");
    retranscribe::request(&ingest, &[BLOCK.to_owned()], &crate::stamp(NOW)).expect("request");
    ingest
        .execute(
            "UPDATE jobs SET done_utc = ?1, result = ?2 WHERE kind = ?3",
            (NOW, LOOP_OVER_SPEECH, Kind::TranscribeSegment),
        )
        .expect("landed again, not yet written");
    assert!(
        retranscribe::candidates(&ingest, 1.0)
            .expect("candidates")
            .is_empty()
    );
}

/// The race of 2026-10-03: the turns pass holds every finished result read
/// when it began, and a request lands while it runs. It must not rewrite the
/// clip from the OLD result it holds and drop the request; it waits for the
/// result that finishes after the request.
#[test]
fn a_request_is_written_only_from_a_result_newer_than_itself() {
    let dir = tempfile::tempdir().expect("tmp");
    let (mut meaning, ingest) = planes(dir.path());
    passes(&mut meaning, &ingest);
    assert_eq!(shown(&meaning), ["old words here"]);
    // The old transcription finished before the request; the request is in,
    // the job still shows its old result (what the pass read at its start).
    ingest
        .execute(
            "UPDATE jobs SET done_utc = '2026-09-27T17:00:00+00:00' WHERE kind = ?1",
            [Kind::TranscribeSegment],
        )
        .expect("an older result");
    ingest
        .execute(
            "INSERT INTO retranscribe_requests (filename, requested_utc) VALUES (?1, ?2)",
            (BLOCK, NOW),
        )
        .expect("the request");

    passes(&mut meaning, &ingest);
    assert_eq!(
        shown(&meaning),
        ["old words here"],
        "nothing rewritten from the old result"
    );
    assert_eq!(
        rows(
            &ingest,
            "SELECT count(*) FROM retranscribe_requests WHERE filename = ?1"
        ),
        1,
        "the request waits"
    );

    lands(&ingest, FRESH);
    passes(&mut meaning, &ingest);
    let now_shown = shown(&meaning);
    assert!(
        now_shown.iter().any(|t| t.contains("new words")),
        "{now_shown:?}"
    );
    assert!(
        !now_shown.iter().any(|t| t.contains("old words")),
        "{now_shown:?}"
    );
}
