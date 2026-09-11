//! What a finished room job means — and, just as much, what it does NOT mean.

use chrono::{TimeZone, Utc};
use recalld::room_turns::{Barren, interpret};

fn block() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 6, 9, 45, 0).unwrap()
}

/// The shape the shim actually returns, copied from a REAL stored result
/// (2026-09-11, job `room-20260906T094500.flac`) rather than invented: `ok` +
/// `result{language, language_confidence, segments[{start,end,text,confidence,
/// avg_logprob,no_speech_prob,words}]}`. A fixture taken from a dataclass
/// instead of the wire is how the segment route once 500'd on every push.
const REAL_SHAPE: &str = r#"{
  "ok": true,
  "result": {
    "language": "nl",
    "language_confidence": null,
    "segments": [
      {"start": 1.5, "end": 3.25, "text": " een twee drie", "confidence": 0.91,
       "avg_logprob": -0.2, "no_speech_prob": 0.01,
       "words": [{"word": "een", "start": 1.5, "end": 1.9}]},
      {"start": 4.0, "end": 5.0, "text": " vier vijf", "confidence": 0.88,
       "avg_logprob": -0.3, "no_speech_prob": 0.02, "words": null}
    ]
  }
}"#;

#[test]
fn offsets_become_absolute_times_against_the_blocks_own_start() {
    let turns = interpret(block(), REAL_SHAPE).expect("turns");

    assert_eq!(turns.len(), 2);
    assert_eq!(
        turns[0].start,
        block() + chrono::Duration::milliseconds(1500)
    );
    assert_eq!(turns[0].end, block() + chrono::Duration::milliseconds(3250));
    // The shim's offsets are relative to the clip it was handed; only the
    // block's filename says when that clip began.
    assert_eq!(turns[1].start, block() + chrono::Duration::seconds(4));
}

#[test]
fn the_text_is_trimmed_and_the_language_rides_along() {
    let turns = interpret(block(), REAL_SHAPE).expect("turns");

    assert_eq!(turns[0].text, "een twee drie");
    assert_eq!(turns[0].language.as_deref(), Some("nl"));
    assert_eq!(turns[0].confidence, Some(0.91));
}

#[test]
fn word_timings_are_carried_verbatim_or_absent_never_invented() {
    let turns = interpret(block(), REAL_SHAPE).expect("turns");

    assert!(turns[0].word_timings.is_some());
    // The second segment's `words` is JSON null, which must land as ABSENT —
    // not as the string "null", which is what a naive `to_string()` of the
    // value would store and what a later reader would then try to parse as
    // timings. Written asserting `Some("null")` first; serde was right.
    assert_eq!(turns[1].word_timings, None);
}

/// ⚠ Transcribing near-silence does not return nothing, it returns INVENTIONS.
/// Measured on this queue: a silent minute came back as "Thank you." twice, and
/// another as 156 segments carrying a 150-character run of tildes at 0.19
/// confidence (#1410). The queue already denies MEASURED silence a job; this is
/// the same rule one stage later, for blocks nobody had measured yet.
#[test]
fn a_turn_with_no_word_in_it_is_dropped_rather_than_stored() {
    let junk = r#"{"ok": true, "result": {"language": "en", "segments": [
        {"start": 0.0, "end": 2.0, "text": "~~~~~~~~~~", "confidence": 0.19},
        {"start": 2.0, "end": 3.0, "text": " ... ", "confidence": 0.2}
    ]}}"#;

    assert_eq!(interpret(block(), junk), Err(Barren::NothingSaid));
}

#[test]
fn a_refusal_is_barren_not_an_error_to_retry() {
    // The shim said the clip is the problem. That is a fact about the audio, and
    // re-running it would produce the same refusal at the same GPU cost.
    let refused = r#"{"ok": false, "error": "unreadable clip"}"#;

    assert_eq!(interpret(block(), refused), Err(Barren::Refused));
}

#[test]
fn a_block_with_nothing_said_yields_no_turns_and_no_complaint() {
    let quiet = r#"{"ok": true, "result": {"language": "en", "segments": []}}"#;

    assert_eq!(interpret(block(), quiet), Err(Barren::NothingSaid));
}

#[test]
fn a_zero_length_turn_is_dropped_because_it_can_hold_no_speech() {
    let degenerate = r#"{"ok": true, "result": {"language": "en", "segments": [
        {"start": 5.0, "end": 5.0, "text": "hello", "confidence": 0.9}
    ]}}"#;

    assert_eq!(interpret(block(), degenerate), Err(Barren::NothingSaid));
}

#[test]
fn a_result_of_another_shape_names_itself_unreadable_rather_than_guessing() {
    match interpret(block(), "not json at all") {
        Err(Barren::Unreadable(why)) => assert!(!why.is_empty(), "the reason is the point"),
        other => panic!("guessed instead of refusing: {other:?}"),
    }
}

/// ⚠ **Run against the REAL 648, not a fixture** — a fixture encodes what I
/// believe the shim returns, and the belief is the thing under test. Point
/// `RECALL_INGEST_DB` at a COPY of the queue (never the live file: a reader on
/// it takes locks the fleet is using) and this reports what the interpreter
/// would produce. It prints COUNTS ONLY — the transcripts are the household's.
///
///     cargo test --test room_turns -- --ignored --nocapture
#[test]
#[ignore = "needs a copy of the fleet's ingest.sqlite via RECALL_INGEST_DB"]
fn every_stored_result_interprets_or_names_why_not() {
    let Ok(path) = std::env::var("RECALL_INGEST_DB") else {
        panic!("set RECALL_INGEST_DB to a COPY of the queue database");
    };
    let conn =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("open the copy read-only");
    let mut stmt = conn
        .prepare("SELECT filename, result FROM jobs WHERE state = 'done' AND result IS NOT NULL")
        .expect("query");
    let rows: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("rows")
        .map(Result::unwrap)
        .collect();

    let (mut turns, mut spoke, mut refused, mut nothing, mut unreadable, mut no_stamp) =
        (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    for (filename, stored) in &rows {
        let Some(start) = block_start_from(filename) else {
            no_stamp += 1;
            continue;
        };
        match interpret(start, stored) {
            Ok(t) => {
                spoke += 1;
                turns += t.len();
            }
            Err(Barren::Refused) => refused += 1,
            Err(Barren::NothingSaid) => nothing += 1,
            Err(Barren::Unreadable(_)) => unreadable += 1,
        }
    }
    println!(
        "results {} → {turns} turns from {spoke} blocks · \
         refused {refused} · nothing said {nothing} · unreadable {unreadable} · \
         unparsable name {no_stamp}",
        rows.len()
    );
    assert!(!rows.is_empty(), "the copy held no finished jobs");
    assert_eq!(unreadable, 0, "a stored result did not match the shape");
    assert_eq!(
        no_stamp, 0,
        "a filename broke the archive's naming contract"
    );
}

/// `room-YYYYMMDDTHHMMSS.flac` → the block's start. The extension is not
/// assumed: the condenser writes `.flac` now and wrote `.opus` before it.
fn block_start_from(filename: &str) -> Option<chrono::DateTime<Utc>> {
    let stamp = filename.strip_prefix("room-")?.split('.').next()?;
    chrono::NaiveDateTime::parse_from_str(stamp, "%Y%m%dT%H%M%S")
        .ok()
        .map(|n| n.and_utc())
}
