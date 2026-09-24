//! What a finished room job means — and, just as much, what it does NOT mean.

use audiocore::job::Kind;
use chrono::{TimeZone, Utc};
use recalld::turns::{Barren, interpret};

fn block() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 6, 9, 45, 0).unwrap()
}

/// The shim's result shape, copied from a real stored result rather than
/// written from a type definition, so it pins the wire format.
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
    // The second segment's `words` is JSON null, which must land as absent,
    // not as the string "null" a naive `to_string()` would store.
    assert_eq!(turns[1].word_timings, None);
}

/// Transcribing near-silence returns inventions, such as runs of tildes at low
/// confidence. The queue denies measured silence a job; this is the same rule
/// one stage later, for blocks nobody measured.
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
    // A refusal is a fact about the audio; re-running would produce the same
    // refusal at the same GPU cost.
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

/// Runs the interpreter over every stored result in a real queue, because a
/// fixture only encodes a belief about the shim. Point `RECALL_INGEST_DB` at a
/// copy of the queue, never the live file (a reader takes locks the fleet
/// uses). It prints counts only; the transcripts are private.
///
///     cargo test --test turns -- --ignored --nocapture
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
/// assumed, because stored blocks exist as both `.flac` and `.opus`.
fn block_start_from(filename: &str) -> Option<chrono::DateTime<Utc>> {
    let stamp = filename.strip_prefix("room-")?.split('.').next()?;
    chrono::NaiveDateTime::parse_from_str(stamp, "%Y%m%dT%H%M%S")
        .ok()
        .map(|n| n.and_utc())
}

// ---- the write plan: the rules that can destroy a person's typed words ----

use recalld::turn_store::Protected;
use recalld::turns::{Standing, plan};

fn t(s: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(s)
        .expect("time")
        .with_timezone(&chrono::Utc)
}

fn room_turn(start: &str, end: &str, text: &str) -> recalld::turns::RoomTurn {
    recalld::turns::RoomTurn {
        start: t(start),
        end: t(end),
        text: text.to_owned(),
        language: Some("nl".to_owned()),
        confidence: Some(0.9),
        word_timings: None,
    }
}

/// A pass's clock, in the stored spelling.
const NOW: &str = "2026-09-11T12:00:00+00:00";
const A: &str = "2026-09-11T10:00:00+00:00";
const B: &str = "2026-09-11T10:00:10+00:00";
const C: &str = "2026-09-11T10:00:20+00:00";
const D: &str = "2026-09-11T10:00:30+00:00";

#[test]
fn a_room_turn_over_a_corrected_span_is_refused_with_a_reason() {
    // The human's text stands. A machine pass does not get to restate it.
    let out = plan(
        vec![room_turn(A, B, "what the model heard")],
        &[],
        &[Protected {
            start: t(A),
            end: t(B),
        }],
    );
    assert!(out.insert.is_empty(), "{:?}", out.insert);
    assert_eq!(out.refused.len(), 1);
    assert!(
        out.refused[0].contains("human-corrected"),
        "{:?}",
        out.refused
    );
}

#[test]
fn a_corrected_per_mic_turn_is_never_hidden_even_when_covered() {
    // A hidden row still stays in transcript_fts, counted and visible to
    // supersession, so hiding a corrected turn is not harmless.
    //
    // Rule 2 is only reachable when a standing turn overlaps a correction while
    // the room turn covering it does not; otherwise rule 1 refuses the room
    // turn and rule 4 hides nothing. So:
    //
    //     correction   A......B
    //     standing 1   A..............C     <- overlaps the correction
    //     standing 2              C......D
    //     room turn           B...........D <- covers both, touches no correction
    let out = plan(
        vec![room_turn(B, D, "the room")],
        &[
            Standing {
                id: 1,
                start: t(A),
                end: t(C),
            }, // a human corrected part of this
            Standing {
                id: 2,
                start: t(C),
                end: t(D),
            }, // plain machine turn
        ],
        &[Protected {
            start: t(A),
            end: t(B),
        }],
    );
    assert_eq!(out.insert.len(), 1, "the room turn misses the correction");
    assert_eq!(out.hide, vec![2], "the partly-corrected turn must survive");
}

#[test]
fn nothing_inserted_means_nothing_hidden() {
    // A pass replaces a transcript or keeps it; it never empties one.
    let out = plan(
        vec![room_turn(A, B, "refused")],
        &[Standing {
            id: 1,
            start: t(A),
            end: t(B),
        }],
        &[Protected {
            start: t(A),
            end: t(B),
        }],
    );
    assert!(out.insert.is_empty());
    assert!(
        out.hide.is_empty(),
        "hiding with nothing to put in its place empties the minute"
    );
}

#[test]
fn a_looping_room_turn_is_swept_and_never_written() {
    // Rule 5 keeps repetition loops out of the system of record, applied where
    // the write is decided rather than on the read path.
    let out = plan(
        vec![
            room_turn(A, B, "momentum momentum momentum momentum"),
            room_turn(C, D, "ik denk dat we dat morgen moeten doen"),
        ],
        &[],
        &[],
    );
    assert_eq!(out.swept, 1);
    assert_eq!(out.insert.len(), 1, "the real sentence survives the sweep");
    assert_eq!(out.insert[0].text, "ik denk dat we dat morgen moeten doen");
}

#[test]
fn a_wordless_room_turn_is_swept_too() {
    let out = plan(vec![room_turn(A, B, "...")], &[], &[]);
    assert_eq!(out.swept, 1);
    assert!(out.insert.is_empty());
}

#[test]
fn a_block_that_is_all_loops_hides_nothing() {
    // Why rule 5 runs before the hide set is built: a minute the room heard as
    // junk must leave the per-mic transcript exactly as it was.
    let out = plan(
        vec![
            room_turn(A, B, "goog goog goog goog goog goog"),
            room_turn(C, D, "***"),
        ],
        &[Standing {
            id: 1,
            start: t(A),
            end: t(D),
        }],
        &[],
    );
    assert_eq!(out.swept, 2);
    assert!(out.insert.is_empty());
    assert!(
        out.hide.is_empty(),
        "a swept block must not hide the microphones that did hear the minute"
    );
}

#[test]
fn an_uncovered_per_mic_turn_is_left_alone() {
    // Only what a written room turn covers is hidden.
    let out = plan(
        vec![room_turn(A, B, "the room")],
        &[Standing {
            id: 9,
            start: t(C),
            end: t(D),
        }],
        &[],
    );
    assert_eq!(out.insert.len(), 1);
    assert!(
        out.hide.is_empty(),
        "a turn outside the room turn's span stays"
    );
}

#[test]
fn the_ordinary_case_writes_and_hides() {
    let out = plan(
        vec![room_turn(A, D, "the whole minute")],
        &[
            Standing {
                id: 1,
                start: t(A),
                end: t(B),
            },
            Standing {
                id: 2,
                start: t(C),
                end: t(D),
            },
        ],
        &[],
    );
    assert_eq!(out.insert.len(), 1);
    assert_eq!(out.hide, vec![1, 2]);
    assert!(out.refused.is_empty());
}

#[test]
fn a_touching_boundary_does_not_count_as_overlap() {
    // Half-open spans: a turn ending exactly where a correction begins does not
    // hit it. Without this every adjacent turn would be treated as corrected.
    let out = plan(
        vec![room_turn(A, B, "before the correction")],
        &[],
        &[Protected {
            start: t(B),
            end: t(C),
        }],
    );
    assert_eq!(out.insert.len(), 1, "{:?}", out.refused);
}

// ---- registering built blocks in the meaning plane ----

use recalld::turns::{ROOM_CHANNELS, ROOM_RATE, register_blocks};

/// The two planes, as two connections — which is what they are in production.
fn two_planes() -> (rusqlite::Connection, rusqlite::Connection) {
    let meaning = rusqlite::Connection::open_in_memory().expect("meaning");
    recalld::meaning_schema::ensure(&meaning).expect("meaning schema");
    let ingest = rusqlite::Connection::open_in_memory().expect("ingest");
    ingest
        .execute_batch(
            "CREATE TABLE segments (filename TEXT PRIMARY KEY, source TEXT NOT NULL,
                 start_utc TEXT NOT NULL, bytes INTEGER NOT NULL, sha256 TEXT NOT NULL,
                 received_utc TEXT NOT NULL, sent_utc TEXT);",
        )
        .expect("ingest schema");
    (meaning, ingest)
}

fn ingest_block(ingest: &rusqlite::Connection, source: &str, filename: &str, start: &str) {
    ingest
        .execute(
            "INSERT INTO segments (filename, source, start_utc, bytes, sha256, received_utc)
             VALUES (?1, ?2, ?3, 1, 'x', '2026-09-11T00:00:00+00:00')",
            (filename, source, start),
        )
        .expect("segment");
}

#[test]
fn a_block_is_registered_with_the_builders_own_shape() {
    let (meaning, ingest) = two_planes();
    ingest_block(
        &ingest,
        "room",
        "room-20260911T100000.flac",
        "2026-09-11T10:00:00+00:00",
    );
    let n = register_blocks(&meaning, &ingest, std::path::Path::new("/data/ingest/room"))
        .expect("register");
    assert_eq!(n, 1);

    let (path, start, end, rate, channels): (String, String, String, i64, i64) = meaning
        .query_row(
            "SELECT path, start_utc, end_utc, sample_rate, channels FROM audio_segments",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .expect("row");
    assert_eq!(path, "/data/ingest/room/room-20260911T100000.flac");
    assert!(start.starts_with("2026-09-11T10:00:00"), "{start}");
    // Exactly one minute: the builder works a UTC-aligned grid.
    assert!(end.starts_with("2026-09-11T10:01:00"), "{end}");
    assert_eq!(rate, ROOM_RATE);
    assert_eq!(channels, ROOM_CHANNELS);
}

#[test]
fn the_room_source_is_registered_as_derived_not_as_a_microphone() {
    // Several views ask `is_device()`. A derived stream registered as a device
    // would be health-checked as a mic with no recorder, and would double-count
    // the microphone it carried.
    let (meaning, ingest) = two_planes();
    ingest_block(
        &ingest,
        "room",
        "room-20260911T100000.flac",
        "2026-09-11T10:00:00+00:00",
    );
    register_blocks(&meaning, &ingest, std::path::Path::new("/x")).expect("register");
    let kind: String = meaning
        .query_row("SELECT kind FROM sources WHERE id = 'room'", [], |r| {
            r.get(0)
        })
        .expect("source");
    assert_eq!(kind, "derived");
}

#[test]
fn the_backfill_is_idempotent_and_meant_to_be_rerun() {
    let (meaning, ingest) = two_planes();
    ingest_block(
        &ingest,
        "room",
        "room-20260911T100000.flac",
        "2026-09-11T10:00:00+00:00",
    );
    let dir = std::path::Path::new("/x");
    assert_eq!(register_blocks(&meaning, &ingest, dir).expect("first"), 1);
    assert_eq!(register_blocks(&meaning, &ingest, dir).expect("again"), 0);
    let n: i64 = meaning
        .query_row("SELECT count(*) FROM audio_segments", [], |r| r.get(0))
        .expect("count");
    assert_eq!(n, 1, "a rerun must not duplicate a block");
}

#[test]
fn only_room_blocks_are_registered() {
    // Microphone segments arrive by push; this must never mint a second row for
    // one of them.
    let (meaning, ingest) = two_planes();
    ingest_block(
        &ingest,
        "usb",
        "usb-20260911T100000.flac",
        "2026-09-11T10:00:00+00:00",
    );
    ingest_block(
        &ingest,
        "room",
        "room-20260911T100000.flac",
        "2026-09-11T10:00:00+00:00",
    );
    assert_eq!(
        register_blocks(&meaning, &ingest, std::path::Path::new("/x")).expect("n"),
        1
    );
    let sources: Vec<String> = meaning
        .prepare("SELECT DISTINCT source_id FROM audio_segments")
        .expect("prep")
        .query_map([], |r| r.get(0))
        .expect("q")
        .collect::<Result<_, _>>()
        .expect("rows");
    assert_eq!(sources, vec!["room".to_owned()]);
}

#[test]
fn a_block_off_the_grid_is_skipped_not_guessed() {
    let (meaning, ingest) = two_planes();
    ingest_block(&ingest, "room", "room-bad.flac", "not-a-time");
    assert_eq!(
        register_blocks(&meaning, &ingest, std::path::Path::new("/x")).expect("n"),
        0
    );
}

#[test]
fn a_blob_lives_under_root_ingest_source_not_root_source() {
    // Blocks live in `<root>/ingest/<source>/`; a path built as
    // `<root>/<source>/` names a directory that does not exist.
    let dir = recalld::store::source_dir(std::path::Path::new("/data"), "room");
    assert_eq!(dir, std::path::Path::new("/data/ingest/room"));
}

// ---- the write itself ----

use recalld::turn_store::HiddenReason;
use recalld::turns::{ROOM, write_block};

fn meaning_with_turns() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().expect("db");
    recalld::meaning_schema::ensure(&conn).expect("schema");
    conn.execute_batch(
        "INSERT INTO sources (id, name, kind) VALUES ('room', 'room', 'room'), ('usb', 'usb', 'coreaudio');
         INSERT INTO audio_segments (id, source_id, path, start_utc, end_utc, sample_rate, channels)
         VALUES (7, 'room', '/room.flac', '2026-09-11T10:00:00+00:00',
                 '2026-09-11T10:01:00+00:00', 16000, 1),
                (3, 'usb', '/usb.opus', '2026-09-11T10:00:00+00:00',
                 '2026-09-11T10:01:00+00:00', 16000, 1);",
    )
    .expect("the room block and a microphone clip");
    conn
}

/// The minute the fixture's turns fall in. `write_block` needs the clip's span
/// to reconcile live turns against it.
fn a_span() -> (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>) {
    (
        "2026-09-11T10:00:00Z".parse().expect("from"),
        "2026-09-11T10:01:00Z".parse().expect("to"),
    )
}

fn a_plan() -> recalld::turns::Plan {
    recalld::turns::Plan {
        insert: vec![room_turn(A, B, "wat zei je")],
        hide: vec![],
        refused: vec![],
        swept: 0,
    }
}

#[test]
fn a_written_turn_is_findable_by_search() {
    // The FTS index is maintained in code; forgetting it fails nothing and
    // makes the text unfindable by search.
    let mut conn = meaning_with_turns();
    assert_eq!(
        write_block(&mut conn, 7, a_span(), &a_plan(), &ROOM, NOW).expect("write"),
        1
    );
    let hits: i64 = conn
        .query_row(
            "SELECT count(*) FROM transcript_fts WHERE transcript_fts MATCH 'zei'",
            [],
            |r| r.get(0),
        )
        .expect("search");
    assert_eq!(hits, 1, "a room turn must be searchable");
}

#[test]
fn a_second_pass_refuses_rather_than_duplicating() {
    // Idempotent by refusing, not overwriting: a re-run must never duplicate,
    // nor "fix" a minute a person has since edited.
    let mut conn = meaning_with_turns();
    let plan = a_plan();
    assert_eq!(
        write_block(&mut conn, 7, a_span(), &plan, &ROOM, NOW).expect("first"),
        1
    );
    assert_eq!(
        write_block(&mut conn, 7, a_span(), &plan, &ROOM, NOW).expect("again"),
        0
    );
    let n: i64 = conn
        .query_row("SELECT count(*) FROM transcript_segments", [], |r| r.get(0))
        .expect("count");
    assert_eq!(n, 1);
}

#[test]
fn hiding_names_a_reason_a_reader_can_act_on() {
    let mut conn = meaning_with_turns();
    conn.execute(
        "INSERT INTO transcript_segments (id, audio_segment_id, start_utc, end_utc, text, asr_model)
         VALUES (99, 3, ?1, ?2, 'per-mic text', 'whisper')",
        (A, B),
    )
    .expect("existing");
    let plan = recalld::turns::Plan {
        insert: vec![room_turn(A, B, "the room heard this")],
        hide: vec![99],
        refused: vec![],
        swept: 0,
    };
    write_block(&mut conn, 7, a_span(), &plan, &ROOM, NOW).expect("write");
    let reason: String = conn
        .query_row(
            "SELECT hidden_reason FROM transcript_segments WHERE id = 99",
            [],
            |r| r.get(0),
        )
        .expect("hidden");
    assert_eq!(reason, HiddenReason::CoveredByRoom.to_string());
}

#[test]
fn an_empty_plan_writes_nothing_and_hides_nothing() {
    let mut conn = meaning_with_turns();
    conn.execute(
        "INSERT INTO transcript_segments (id, audio_segment_id, start_utc, end_utc, text, asr_model)
         VALUES (99, 3, ?1, ?2, 'per-mic text', 'whisper')",
        (A, B),
    )
    .expect("existing");
    let empty = recalld::turns::Plan {
        insert: vec![],
        hide: vec![99],
        refused: vec![],
        swept: 0,
    };
    assert_eq!(
        write_block(&mut conn, 7, a_span(), &empty, &ROOM, NOW).expect("write"),
        0
    );
    let reason: Option<String> = conn
        .query_row(
            "SELECT hidden_reason FROM transcript_segments WHERE id = 99",
            [],
            |r| r.get(0),
        )
        .expect("row");
    assert!(
        reason.is_none(),
        "a hide with nothing to replace it empties the minute"
    );
}

// ---- the pass's progress ledger ----

use recalld::turns::{PER_MIC, Pass, write_pass};

/// Both planes, built by their real schemas, for a real `write_pass`.
fn planes_for_a_pass() -> (
    rusqlite::Connection,
    rusqlite::Connection,
    tempfile::TempDir,
) {
    let meaning = rusqlite::Connection::open_in_memory().expect("meaning");
    recalld::meaning_schema::ensure(&meaning).expect("meaning schema");
    // Every clip's source is registered, as the foreign key requires; these
    // tests name sources freely.
    meaning
        .execute_batch(
            "CREATE TEMP TRIGGER known_source BEFORE INSERT ON audio_segments BEGIN
                 INSERT OR IGNORE INTO sources (id, name, kind) VALUES (
                     NEW.source_id, NEW.source_id,
                     CASE WHEN NEW.source_id = 'room' THEN 'room'
                          WHEN NEW.source_id LIKE 'meeting-%' THEN 'upload'
                          ELSE 'coreaudio' END);
             END;",
        )
        .expect("sources");
    // The ingest plane is opened through `store::open`, not copied DDL:
    // `write_pass` joins `jobs` to `segments`, so the test must use
    // production's table.
    let dir = tempfile::tempdir().expect("tmp");
    let ingest = recalld::store::open(dir.path()).expect("ingest");
    recalld::ingest_schema::ensure(&ingest).expect("jobs");
    (meaning, ingest, dir)
}

/// A done `transcribe-room` job, and the room block it belongs to.
fn done_room_job(
    meaning: &rusqlite::Connection,
    ingest: &rusqlite::Connection,
    filename: &str,
    start_utc: &str,
    result: &str,
) {
    done_job(
        ingest,
        Kind::TranscribeRoom,
        "room",
        filename,
        start_utc,
        result,
    );
    meaning
        .execute(
            "INSERT OR IGNORE INTO audio_segments
                 (source_id, path, start_utc, end_utc, sample_rate, channels)
             VALUES ('room', '/x', ?1, ?2, 16000, 1)",
            (start_utc, &minute_later(start_utc)),
        )
        .expect("block");
}

/// A done job and the ingest-plane segment row it names. `write_pass` joins
/// them to learn the source, and every real job derives from a `segments` row.
fn done_job(
    ingest: &rusqlite::Connection,
    kind: Kind,
    source: &str,
    filename: &str,
    start_utc: &str,
    result: &str,
) {
    ingest
        .execute(
            "INSERT INTO jobs (kind, filename, state, created_utc, done_utc, result)
             VALUES (?1, ?2, 'done', '2026-09-11T00:00:00Z',
                     '2026-09-11T00:01:00Z', ?3)",
            (kind, filename, result),
        )
        .expect("job");
    ingest
        .execute(
            "INSERT OR IGNORE INTO segments
                 (filename, source, start_utc, bytes, sha256, received_utc)
             VALUES (?1, ?2, ?3, 1, 'x', ?3)",
            (filename, source, start_utc),
        )
        .expect("segment");
}

/// `start_utc` plus a minute, keeping the archive's own spelling.
fn minute_later(start_utc: &str) -> String {
    let start = chrono::DateTime::parse_from_rfc3339(start_utc).expect("start");
    (start + chrono::Duration::seconds(60)).to_rfc3339_opts(chrono::SecondsFormat::Micros, false)
}

fn a_result(text: &str) -> String {
    format!(
        r#"{{"ok": true, "result": {{"language": "nl", "segments": [
             {{"start": 0.0, "end": 5.0, "text": "{text}", "confidence": 0.9}}]}}}}"#
    )
}

#[test]
fn a_block_that_writes_nothing_is_decided_once_not_every_pass() {
    // A block that writes nothing leaves no trace in the meaning plane to
    // derive "done" from, so only the ledger can retire it. Without it the pass
    // re-examines the same blocks forever.
    let (mut meaning, ingest, _dir) = planes_for_a_pass();
    done_room_job(
        &meaning,
        &ingest,
        "room-20260911T100000.flac",
        "2026-09-11T10:00:00+00:00",
        // A repetition loop: swept by rule 5, so nothing is written.
        &a_result("momentum momentum momentum momentum"),
    );

    let first = write_pass(&mut meaning, &ingest, &ROOM, NOW, 20).expect("first");
    assert_eq!(first.blocks, 1, "the block is examined once");
    assert_eq!(first.swept, 1);
    assert_eq!(first.turns, 0);

    let second = write_pass(&mut meaning, &ingest, &ROOM, NOW, 20).expect("second");
    assert_eq!(
        second,
        Pass::default(),
        "a decided block must not be reconsidered — this is the bug the ledger fixes"
    );
}

#[test]
fn a_written_block_is_retired_by_its_turns_and_not_by_the_ledger() {
    // Deleting the room turns is enough to re-enable the block, with no ledger
    // row to clear.
    let (mut meaning, ingest, _dir) = planes_for_a_pass();
    done_room_job(
        &meaning,
        &ingest,
        "room-20260911T100000.flac",
        "2026-09-11T10:00:00+00:00",
        &a_result("ik denk dat we dat morgen moeten doen"),
    );

    assert_eq!(
        write_pass(&mut meaning, &ingest, &ROOM, NOW, 20)
            .expect("first")
            .turns,
        1
    );
    let ledgered: i64 = ingest
        .query_row("SELECT count(*) FROM pass_ledger", [], |r| r.get(0))
        .expect("ledger");
    assert_eq!(
        ledgered, 0,
        "a written block needs no row; its turns are it"
    );

    assert_eq!(
        write_pass(&mut meaning, &ingest, &ROOM, NOW, 20).expect("second"),
        Pass::default(),
        "and it is not rewritten while those turns stand"
    );

    // The reversal, meaning plane only.
    meaning
        .execute(
            "DELETE FROM transcript_segments WHERE provenance = 'room'",
            [],
        )
        .expect("reverse");
    assert_eq!(
        write_pass(&mut meaning, &ingest, &ROOM, NOW, 20)
            .expect("after reversal")
            .turns,
        1,
        "deleting the turns must make the block eligible again by itself"
    );
}

#[test]
fn a_block_whose_audio_is_not_registered_yet_comes_back() {
    // The one barren cause that gets no ledger row: the registrar runs in its
    // own loop, so a block examined too early is not a verdict, and a row would
    // retire it for good.
    let (mut meaning, ingest, _dir) = planes_for_a_pass();
    done_job(
        &ingest,
        Kind::TranscribeRoom,
        "room",
        "room-20260911T100000.flac",
        "2026-09-11T10:00:00+00:00",
        &a_result("wat zei je"),
    );

    let early = write_pass(&mut meaning, &ingest, &ROOM, NOW, 20).expect("early");
    assert_eq!(early.barren, 1);
    assert_eq!(early.blocks, 0);
    let ledgered: i64 = ingest
        .query_row("SELECT count(*) FROM pass_ledger", [], |r| r.get(0))
        .expect("ledger");
    assert_eq!(ledgered, 0, "waiting is not deciding");

    // The registrar catches up.
    meaning
        .execute(
            "INSERT INTO audio_segments
                 (source_id, path, start_utc, end_utc, sample_rate, channels)
             VALUES ('room', '/x', '2026-09-11T10:00:00+00:00',
                     '2026-09-11T10:01:00+00:00', 16000, 1)",
            [],
        )
        .expect("block");
    assert_eq!(
        write_pass(&mut meaning, &ingest, &ROOM, NOW, 20)
            .expect("later")
            .turns,
        1,
        "the block must still be reachable once its audio exists"
    );
}

#[test]
fn the_limit_counts_blocks_decided_not_rows_looked_at() {
    // Why the SQL has no LIMIT: with decided blocks ahead in filename order, a
    // query limited to N returns N ineligible rows and the pass never moves.
    // `limit` bounds the work, not the read.
    let (mut meaning, ingest, _dir) = planes_for_a_pass();
    for minute in 0..5 {
        done_room_job(
            &meaning,
            &ingest,
            &format!("room-20260911T10{minute:02}00.flac"),
            &format!("2026-09-11T10:{minute:02}:00+00:00"),
            &a_result("goog goog goog goog goog goog"), // swept: writes nothing
        );
    }
    done_room_job(
        &meaning,
        &ingest,
        "room-20260911T105900.flac",
        "2026-09-11T10:59:00+00:00",
        &a_result("dit is echte spraak"),
    );

    // Retire the five junk blocks first, one pass at a time.
    for _ in 0..5 {
        write_pass(&mut meaning, &ingest, &ROOM, NOW, 1).expect("pass");
    }
    let reached = write_pass(&mut meaning, &ingest, &ROOM, NOW, 1).expect("reach");
    assert_eq!(
        reached.turns, 1,
        "the real block must be reachable past the decided ones"
    );
}

// ---- the per-mic stream ----

/// A microphone clip: the ingest-plane blob, the job, and the meaning-plane row
/// its turns will hang from. `end_utc` is a real span, because the pass reads it.
fn done_mic_job(
    meaning: &rusqlite::Connection,
    ingest: &rusqlite::Connection,
    source: &str,
    filename: &str,
    start_utc: &str,
    end_utc: &str,
    result: &str,
) {
    done_job(
        ingest,
        Kind::TranscribeSegment,
        source,
        filename,
        start_utc,
        result,
    );
    meaning
        .execute(
            "INSERT OR IGNORE INTO sources (id, name, kind) VALUES (?1, ?1, 'coreaudio')",
            [source],
        )
        .expect("source");
    meaning
        .execute(
            "INSERT OR IGNORE INTO audio_segments
                 (source_id, path, start_utc, end_utc, sample_rate, channels)
             VALUES (?1, '/x', ?2, ?3, 48000, 1)",
            (source, start_utc, end_utc),
        )
        .expect("clip");
}

#[test]
fn a_per_mic_pass_writes_its_turns_and_hides_absolutely_nothing() {
    // A room turn stands in for the microphones and hides what it covers; a
    // per-mic turn stands for its own clip and covers nothing. With
    // `hides_covered` true here, one microphone would suppress the others.
    let (mut meaning, ingest, _dir) = planes_for_a_pass();
    done_mic_job(
        &meaning,
        &ingest,
        "usb",
        "usb-20260911T100000.flac",
        "2026-09-11T10:00:00+00:00",
        "2026-09-11T10:01:00+00:00",
        &a_result("we moeten dat morgen opschrijven"),
    );
    // Another microphone already transcribed the same minute. Standing, visible.
    meaning
        .execute(
            "INSERT INTO sources (id, name, kind) VALUES ('geb', 'geb', 'tcp_pcm')",
            [],
        )
        .expect("other source");
    meaning
        .execute(
            "INSERT INTO audio_segments (id, source_id, path, start_utc, end_utc,
                                         sample_rate, channels)
             VALUES (500, 'geb', '/y', '2026-09-11T10:00:00+00:00',
                     '2026-09-11T10:01:00+00:00', 48000, 1)",
            [],
        )
        .expect("other clip");
    meaning
        .execute(
            "INSERT INTO transcript_segments (id, audio_segment_id, start_utc, end_utc,
                                              text, asr_model)
             VALUES (501, 500, '2026-09-11T10:00:01+00:00', '2026-09-11T10:00:04+00:00',
                     'we moeten dat morgen opschrijven', 'whisper')",
            [],
        )
        .expect("other turn");

    let pass = write_pass(&mut meaning, &ingest, &PER_MIC, NOW, 20).expect("pass");
    assert_eq!(pass.turns, 1);
    assert_eq!(pass.hidden, 0, "a per-mic turn must hide nothing, ever");

    let still_visible: Option<String> = meaning
        .query_row(
            "SELECT hidden_reason FROM transcript_segments WHERE id = 501",
            [],
            |r| r.get(0),
        )
        .expect("other turn");
    assert_eq!(still_visible, None);
}

#[test]
fn a_per_mic_turn_records_the_provenance_that_takes_it_back_and_the_corpus_model() {
    // `asr_model` matches the rest of the corpus, so the archive does not split
    // by orchestrator; `provenance` is unique to this pass, so reversing it is
    // one DELETE that names only its rows.
    let (mut meaning, ingest, _dir) = planes_for_a_pass();
    done_mic_job(
        &meaning,
        &ingest,
        "usb",
        "usb-20260911T100000.flac",
        "2026-09-11T10:00:00+00:00",
        "2026-09-11T10:01:00+00:00",
        &a_result("dit is echte spraak"),
    );
    write_pass(&mut meaning, &ingest, &PER_MIC, NOW, 20).expect("pass");

    let (model, provenance): (String, String) = meaning
        .query_row(
            "SELECT asr_model, provenance FROM transcript_segments
             WHERE provenance IS NOT NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("row");
    assert_eq!(model, "mlx-community/whisper-large-v3-turbo");
    assert_eq!(provenance, "per-mic (runner)");
    assert_ne!(
        provenance, "room",
        "the two streams must not share a reversal key"
    );
}

#[test]
fn a_clip_the_mac_already_transcribed_is_left_alone() {
    // Both orchestrators may run at once. Wasted GPU is acceptable; a
    // duplicated transcript is not, because a reader cannot tell which is which.
    let (mut meaning, ingest, _dir) = planes_for_a_pass();
    done_mic_job(
        &meaning,
        &ingest,
        "usb",
        "usb-20260911T100000.flac",
        "2026-09-11T10:00:00+00:00",
        "2026-09-11T10:01:00+00:00",
        &a_result("dit is echte spraak"),
    );
    let clip: i64 = meaning
        .query_row(
            "SELECT id FROM audio_segments WHERE source_id = 'usb'",
            [],
            |r| r.get(0),
        )
        .expect("clip");
    meaning
        .execute(
            "INSERT INTO transcript_segments (audio_segment_id, start_utc, end_utc,
                                              text, asr_model)
             VALUES (?1, '2026-09-11T10:00:01+00:00', '2026-09-11T10:00:04+00:00',
                     'dit is echte spraak', 'mlx-community/whisper-large-v3-turbo')",
            [clip],
        )
        .expect("mac turn");

    let pass = write_pass(&mut meaning, &ingest, &PER_MIC, NOW, 20).expect("pass");
    assert_eq!(pass.turns, 0);
    let rows: i64 = meaning
        .query_row(
            "SELECT count(*) FROM transcript_segments WHERE audio_segment_id = ?1",
            [clip],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(rows, 1, "the Mac's turn stands alone");
}

#[test]
fn a_per_mic_turn_over_a_human_correction_is_refused_like_a_room_turn() {
    // Rule 1 belongs to the archive, not the room stream: what a person typed
    // is the one thing not re-derivable from audio.
    let (mut meaning, ingest, _dir) = planes_for_a_pass();
    done_mic_job(
        &meaning,
        &ingest,
        "usb",
        "usb-20260911T100000.flac",
        "2026-09-11T10:00:00+00:00",
        "2026-09-11T10:01:00+00:00",
        &a_result("dit is echte spraak"),
    );
    meaning
        .execute(
            "INSERT INTO corrections (start_utc, end_utc, original_text,
                                      corrected_text, created_utc)
             VALUES ('2026-09-11T10:00:00+00:00', '2026-09-11T10:00:30+00:00',
                     'was', 'is', '2026-09-11T12:00:00+00:00')",
            [],
        )
        .expect("correction");

    let pass = write_pass(&mut meaning, &ingest, &PER_MIC, NOW, 20).expect("pass");
    assert_eq!(pass.turns, 0);
    assert_eq!(pass.refused, 1);
}

#[test]
fn the_correction_window_is_the_clips_own_span_not_a_minute() {
    // A microphone clip is whatever the segment muxer closed, so it can be
    // longer than a minute. Assuming the room's 60 s grid would end the window
    // early, before the correction it is about to overwrite.
    let (mut meaning, ingest, _dir) = planes_for_a_pass();
    done_mic_job(
        &meaning,
        &ingest,
        "usb",
        "usb-20260911T100000.flac",
        "2026-09-11T10:00:00+00:00",
        "2026-09-11T10:05:00+00:00", // five minutes, not one
        r#"{"ok": true, "result": {"language": "nl", "segments": [
             {"start": 200.0, "end": 210.0, "text": "dit is echte spraak",
              "confidence": 0.9}]}}"#,
    );
    meaning
        .execute(
            "INSERT INTO corrections (start_utc, end_utc, original_text,
                                      corrected_text, created_utc)
             VALUES ('2026-09-11T10:03:18+00:00', '2026-09-11T10:03:28+00:00',
                     'was', 'is', '2026-09-11T12:00:00+00:00')",
            [],
        )
        .expect("correction");

    let pass = write_pass(&mut meaning, &ingest, &PER_MIC, NOW, 20).expect("pass");
    assert_eq!(
        pass.refused, 1,
        "a correction 3m18s into the clip is inside it, and must be seen"
    );
    assert_eq!(pass.turns, 0);
}

#[test]
fn the_source_is_read_from_the_ingest_plane_not_split_out_of_the_filename() {
    // `meeting-20260907-0905` is a source id, so no split of a filename on a
    // hyphen is safe; a wrong split makes the clip silently barren forever.
    let (mut meaning, ingest, _dir) = planes_for_a_pass();
    done_mic_job(
        &meaning,
        &ingest,
        "meeting-20260907-0905",
        "meeting-20260907-0905-20260907T090500.opus",
        "2026-09-07T09:05:00+00:00",
        "2026-09-07T09:06:00+00:00",
        &a_result("dit is echte spraak"),
    );

    let pass = write_pass(&mut meaning, &ingest, &PER_MIC, NOW, 20).expect("pass");
    assert_eq!(pass.turns, 1, "the hyphenated source must resolve");
}

#[test]
fn one_streams_refusal_does_not_retire_the_others_job() {
    // The ledger is keyed on (kind, filename). A shared key would let a room
    // block that swept to nothing silently retire a per-mic clip of the same
    // name.
    let (mut meaning, ingest, _dir) = planes_for_a_pass();
    let name = "usb-20260911T100000.flac";
    done_mic_job(
        &meaning,
        &ingest,
        "usb",
        name,
        "2026-09-11T10:00:00+00:00",
        "2026-09-11T10:01:00+00:00",
        &a_result("dit is echte spraak"),
    );
    // The same filename decided by the OTHER stream, writing nothing.
    recalld::ingest_schema::ensure(&ingest).expect("ledger");
    ingest
        .execute(
            "INSERT INTO pass_ledger (kind, filename, outcome, decided_utc)
             VALUES ('transcribe-room', ?1, 'nothing-to-write', 'then')",
            [name],
        )
        .expect("other verdict");

    let pass = write_pass(&mut meaning, &ingest, &PER_MIC, NOW, 20).expect("pass");
    assert_eq!(pass.turns, 1, "the per-mic job is still its own to decide");
}

#[test]
fn a_per_mic_turn_names_the_model_the_shim_will_actually_load() {
    // The queue carries no model field, so the shim loads its own default and
    // `SHIM_MODEL` must match it, or every per-mic turn names the wrong model.
    // `asr.py` is what the running shim reads, so the test reads it too.
    let asr = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/recall/asr.py"),
    )
    .expect("asr.py");
    let line = asr
        .lines()
        .find(|l| l.starts_with("DEFAULT_MODEL"))
        .expect("DEFAULT_MODEL is gone from asr.py — this coupling needs rethinking");
    let quoted = line
        .split('"')
        .nth(1)
        .expect("DEFAULT_MODEL is no longer a plain string literal");
    assert_eq!(
        quoted,
        recalld::turns::SHIM_MODEL,
        "recalld records `{}` on every per-mic turn, but the shim loads `{quoted}`",
        recalld::turns::SHIM_MODEL
    );
}

// ---- the segment registrar ----

use recalld::turns::{PassKind, register_segments};

/// An ingest-plane blob and a real audio file where the registrar looks for
/// it. Real, because the pass decodes it to measure the duration.
fn ingest_blob(root: &std::path::Path, source: &str, stamp: &str, seconds: f64) -> String {
    let filename = format!("{source}-{stamp}.flac");
    let dir = recalld::store::source_dir(root, source);
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join(&filename);
    let status = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("sine=frequency=440:duration={seconds}"))
        .args(["-ar", "48000", "-ac", "1", "-y"])
        .arg(&path)
        .status()
        .expect("ffmpeg");
    assert!(status.success(), "ffmpeg could not write the fixture");
    let conn = recalld::store::open(root).expect("ingest");
    conn.execute(
        "INSERT OR IGNORE INTO segments
             (filename, source, start_utc, bytes, sha256, received_utc)
         VALUES (?1, ?2, ?3, 1, 'x', ?3)",
        (
            &filename,
            source,
            format!(
                "{}-{}-{}T{}:{}:{}Z",
                &stamp[0..4],
                &stamp[4..6],
                &stamp[6..8],
                &stamp[9..11],
                &stamp[11..13],
                &stamp[13..15]
            ),
        ),
    )
    .expect("segment");
    filename
}

/// The meaning plane, with `usb` known as a microphone.
fn meaning_for_registration() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().expect("mem");
    recalld::meaning_schema::ensure(&conn).expect("schema");
    conn.execute(
        "INSERT INTO sources (id, name, kind) VALUES ('usb', 'usb', 'coreaudio')",
        [],
    )
    .expect("usb");
    conn
}

#[test]
fn a_clip_is_registered_with_the_duration_it_actually_has() {
    // Measured, not assumed: `write_pass` sizes its correction window from
    // `end_utc`, and a window too narrow for a long clip overwrites typed words.
    let dir = tempfile::tempdir().expect("tmp");
    ingest_blob(dir.path(), "usb", "20260913T100000", 7.5);
    let meaning = meaning_for_registration();
    let ingest = recalld::store::open(dir.path()).expect("ingest");

    let pass = register_segments(&meaning, &ingest, dir.path(), "now", 10).expect("register");
    assert_eq!(pass.added, 1);

    let (start, end, rate, channels): (String, String, i64, i64) = meaning
        .query_row(
            "SELECT start_utc, end_utc, sample_rate, channels FROM audio_segments",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .expect("row");
    assert_eq!(rate, 48000);
    assert_eq!(channels, 1);
    let span = (chrono::DateTime::parse_from_rfc3339(&end).expect("end")
        - chrono::DateTime::parse_from_rfc3339(&start).expect("start"))
    .num_milliseconds();
    assert!(
        (7400..=7600).contains(&span),
        "span {span}ms should be the clip's real 7.5s, not a nominal minute"
    );
}

#[test]
fn the_registered_path_is_the_ingest_copy_that_is_complete() {
    // A copy of the same bytes may exist at `<root>/<source>/`, but it is
    // incomplete; rows must point at the ingest copy.
    let dir = tempfile::tempdir().expect("tmp");
    let name = ingest_blob(dir.path(), "usb", "20260913T100000", 1.0);
    let meaning = meaning_for_registration();
    let ingest = recalld::store::open(dir.path()).expect("ingest");
    register_segments(&meaning, &ingest, dir.path(), "now", 10).expect("register");

    let path: String = meaning
        .query_row("SELECT path FROM audio_segments", [], |r| r.get(0))
        .expect("row");
    assert!(path.ends_with(&format!("ingest/usb/{name}")), "got {path}");
    assert!(std::path::Path::new(&path).is_file(), "and it must exist");
}

#[test]
fn a_clip_already_registered_is_skipped_before_any_statement_runs() {
    // The first of two defences: the `have` set holds every
    // `audio_segments.path` basename, so a registered clip never reaches the
    // INSERT. The SQL's `OR IGNORE` is invisible from here; see
    // a_row_whose_filename_changed_is_still_not_repointed.
    let dir = tempfile::tempdir().expect("tmp");
    ingest_blob(dir.path(), "usb", "20260913T100000", 1.0);
    let meaning = meaning_for_registration();
    meaning
        .execute(
            "INSERT INTO audio_segments (source_id, path, start_utc, end_utc,
                                         sample_rate, channels)
             VALUES ('usb', '/data/usb/usb-20260913T100000.flac',
                     '2026-09-13T10:00:00+00:00', '2026-09-13T10:01:00+00:00', 48000, 1)",
            [],
        )
        .expect("existing");
    let ingest = recalld::store::open(dir.path()).expect("ingest");

    let pass = register_segments(&meaning, &ingest, dir.path(), "now", 10).expect("register");
    assert_eq!(pass.added, 0);
    let path: String = meaning
        .query_row("SELECT path FROM audio_segments", [], |r| r.get(0))
        .expect("row");
    assert_eq!(path, "/data/usb/usb-20260913T100000.flac");
}

#[test]
fn a_clip_ffmpeg_cannot_read_is_retired_not_retried_for_ever() {
    // Header-only clips exist in the archive. Without a ledger row each would
    // cost a decode attempt on every pass, for ever.
    let dir = tempfile::tempdir().expect("tmp");
    let source_dir = recalld::store::source_dir(dir.path(), "usb");
    std::fs::create_dir_all(&source_dir).expect("dir");
    std::fs::write(source_dir.join("usb-20260913T100000.flac"), b"fLaC").expect("tombstone");
    let ingest = recalld::store::open(dir.path()).expect("ingest");
    ingest
        .execute(
            "INSERT INTO segments (filename, source, start_utc, bytes, sha256, received_utc)
             VALUES ('usb-20260913T100000.flac', 'usb', '2026-09-13T10:00:00Z', 4, 'x',
                     '2026-09-13T10:00:00Z')",
            [],
        )
        .expect("segment");
    let meaning = meaning_for_registration();

    let first = register_segments(&meaning, &ingest, dir.path(), "now", 10).expect("first");
    assert_eq!(first.unreadable, 1);
    assert_eq!(first.added, 0);

    let second = register_segments(&meaning, &ingest, dir.path(), "now", 10).expect("second");
    assert_eq!(
        second,
        recalld::turns::Registered::default(),
        "a tombstone must be decided once, not decoded on every pass"
    );
    let outcome: String = ingest
        .query_row(
            "SELECT outcome FROM pass_ledger WHERE kind = ?1",
            [PassKind::Register],
            |r| r.get(0),
        )
        .expect("ledger");
    assert_eq!(outcome, "unreadable");
}

#[test]
fn a_source_the_meaning_plane_cannot_type_waits_rather_than_being_guessed() {
    // `sources.kind` says how a recorder produces PCM and the sender owns it.
    // Sources are inserted with INSERT OR IGNORE, so a guess would be permanent;
    // an unknown source is counted and left.
    let dir = tempfile::tempdir().expect("tmp");
    ingest_blob(dir.path(), "newmic", "20260913T100000", 1.0);
    let meaning = meaning_for_registration(); // knows `usb` only
    let ingest = recalld::store::open(dir.path()).expect("ingest");

    let pass = register_segments(&meaning, &ingest, dir.path(), "now", 10).expect("register");
    assert_eq!(pass.waiting, 1);
    assert_eq!(pass.added, 0);
    let rows: i64 = meaning
        .query_row("SELECT count(*) FROM sources", [], |r| r.get(0))
        .expect("sources");
    assert_eq!(rows, 1, "and no source was invented");
}

#[test]
fn a_row_whose_filename_changed_is_still_not_repointed() {
    // The second defence. The `have` short-circuit is keyed on the basename,
    // the table on `UNIQUE (source_id, start_utc)`; when a stored path's
    // filename differs, `have` misses and only `OR IGNORE` stops the INSERT
    // repointing a row out from under playable turns.
    let dir = tempfile::tempdir().expect("tmp");
    ingest_blob(dir.path(), "usb", "20260913T100000", 1.0);
    let meaning = meaning_for_registration();
    meaning
        .execute(
            "INSERT INTO audio_segments (source_id, path, start_utc, end_utc,
                                         sample_rate, channels)
             VALUES ('usb', '/data/usb/renamed-by-something-else.flac',
                     '2026-09-13T10:00:00+00:00', '2026-09-13T10:01:00+00:00', 48000, 1)",
            [],
        )
        .expect("existing");
    let ingest = recalld::store::open(dir.path()).expect("ingest");

    let pass = register_segments(&meaning, &ingest, dir.path(), "now", 10).expect("register");
    assert_eq!(pass.added, 0, "the unique constraint must absorb it");
    let (rows, path): (i64, String) = meaning
        .query_row("SELECT count(*), max(path) FROM audio_segments", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .expect("rows");
    assert_eq!(
        rows, 1,
        "and must not mint a second row for the same minute"
    );
    assert_eq!(
        path, "/data/usb/renamed-by-something-else.flac",
        "the path a turn can already play must stand"
    );
}

#[test]
fn a_per_mic_write_hides_the_live_guess_it_replaces() {
    // A live turn is a guess made while somebody was still speaking. A runner
    // writing turns directly bypasses the push path's reconciliation, so it
    // must hide the guess itself, or the timeline shows the conversation twice.
    let (mut meaning, ingest, _dir) = planes_for_a_pass();
    done_mic_job(
        &meaning,
        &ingest,
        "usb",
        "usb-20260911T100000.flac",
        "2026-09-11T10:00:00+00:00",
        "2026-09-11T10:01:00+00:00",
        &a_result("dit is echte spraak"),
    );
    meaning
        .execute(
            "INSERT INTO transcript_segments (id, audio_segment_id, start_utc, end_utc,
                                              text, asr_model)
             VALUES (900, NULL, '2026-09-11T10:00:12+00:00', '2026-09-11T10:00:15+00:00',
                     'dit is ecte spraak', 'live')",
            [],
        )
        .expect("live turn");

    write_pass(&mut meaning, &ingest, &PER_MIC, NOW, 20).expect("pass");

    let hidden: Option<String> = meaning
        .query_row(
            "SELECT hidden_reason FROM transcript_segments WHERE id = 900",
            [],
            |r| r.get(0),
        )
        .expect("live turn");
    assert_eq!(hidden.as_deref(), Some("live-reconciled"));
}

#[test]
fn a_live_turn_outside_the_clip_is_left_alone() {
    // A live turn has no `audio_segment_id`; only the span relates it to a
    // clip. A bound past the clip would hide a guess for a minute nothing has
    // transcribed yet, losing the only record of it.
    let (mut meaning, ingest, _dir) = planes_for_a_pass();
    done_mic_job(
        &meaning,
        &ingest,
        "usb",
        "usb-20260911T100000.flac",
        "2026-09-11T10:00:00+00:00",
        "2026-09-11T10:01:00+00:00",
        &a_result("dit is echte spraak"),
    );
    meaning
        .execute(
            "INSERT INTO transcript_segments (id, audio_segment_id, start_utc, end_utc,
                                              text, asr_model)
             VALUES (901, NULL, '2026-09-11T10:01:30+00:00', '2026-09-11T10:01:33+00:00',
                     'een latere zin', 'live')",
            [],
        )
        .expect("later live turn");

    write_pass(&mut meaning, &ingest, &PER_MIC, NOW, 20).expect("pass");

    let hidden: Option<String> = meaning
        .query_row(
            "SELECT hidden_reason FROM transcript_segments WHERE id = 901",
            [],
            |r| r.get(0),
        )
        .expect("live turn");
    assert_eq!(
        hidden, None,
        "a guess for a minute nobody has transcribed must stand"
    );
}

/// `register_segments` decodes a clip in full, then inserts with `OR IGNORE`
/// on `(source_id, start_utc)`. Where two files share a minute the second
/// insert is ignored, and without a ledger row that clip would be decoded again
/// on every pass. A pass ledgers every terminal decision, including "nothing to
/// do".
#[test]
fn a_clip_whose_minute_a_sibling_already_holds_is_retired_not_reconsidered() {
    let dir = tempfile::tempdir().expect("tmp");
    let meaning = meaning_for_registration();
    let ingest = recalld::store::open(dir.path()).expect("ingest");

    // Two files, one minute, as the phones produce.
    ingest_blob(dir.path(), "usb", "20260913T100000", 3.0);
    let twin = twin_blob(dir.path(), "usb", "20260913T100000", 3.0, "wav");

    let first = register_segments(&meaning, &ingest, dir.path(), "now", 10).expect("first");
    assert_eq!(first.added, 1, "one of the two takes the row");
    assert_eq!(first.covered, 1, "the other is covered by its sibling");
    // The sibling's start time is in its name and the minute is registered,
    // so it is decided without a decode.
    assert_eq!(
        first.probed, 1,
        "only the clip that took the row may be decoded; the sibling costs nothing"
    );

    // Both are ledgered, so neither is a candidate again.
    let ledgered: i64 = ingest
        .query_row(
            "SELECT count(*) FROM pass_ledger WHERE kind = ?1",
            [PassKind::Register],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(ledgered, 2, "every terminal decision writes a ledger row");
    let outcome: String = ingest
        .query_row(
            "SELECT outcome FROM pass_ledger WHERE kind = ?1 AND filename = ?2",
            (PassKind::Register, &twin),
            |r| r.get(0),
        )
        .expect("the twin's row");
    assert_eq!(
        outcome, "covered-by-sibling",
        "and it says WHY, not just that"
    );

    // The second pass does nothing: no probe, no insert.
    let second = register_segments(&meaning, &ingest, dir.path(), "now", 10).expect("second");
    assert_eq!(
        (second.added, second.covered, second.retired, second.probed),
        (0, 0, 0, 0),
        "a decided clip is never looked at again, and nothing is decoded"
    );
}

/// A second file for the same minute in another container, as a phone that
/// uploads both a `.wav` and its transcode leaves.
fn twin_blob(root: &std::path::Path, source: &str, stamp: &str, seconds: f64, ext: &str) -> String {
    let filename = format!("{source}-{stamp}.{ext}");
    let dir = recalld::store::source_dir(root, source);
    let path = dir.join(&filename);
    let status = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("sine=frequency=440:duration={seconds}"))
        .args(["-ar", "48000", "-ac", "1", "-y"])
        .arg(&path)
        .status()
        .expect("ffmpeg");
    assert!(status.success(), "ffmpeg could not write the twin");
    let conn = recalld::store::open(root).expect("ingest");
    conn.execute(
        "INSERT OR IGNORE INTO segments
             (filename, source, start_utc, bytes, sha256, received_utc)
         VALUES (?1, ?2, ?3, 1, 'x', ?3)",
        (
            &filename,
            source,
            format!(
                "{}-{}-{}T{}:{}:{}Z",
                &stamp[0..4],
                &stamp[4..6],
                &stamp[6..8],
                &stamp[9..11],
                &stamp[11..13],
                &stamp[13..15]
            ),
        ),
    )
    .expect("row");
    filename
}

#[test]
fn a_block_whose_session_was_deleted_is_decided_not_waited_for() {
    // No audio is transient for a clip not yet registered and permanent for a
    // deleted session, whose ingest rows and finished job outlive the deletion.
    // The tombstone journal tells the two apart.
    let (mut meaning, ingest, _dir) = planes_for_a_pass();
    done_job(
        &ingest,
        Kind::TranscribeSegment,
        "meeting-20260917-1500",
        "meeting-20260917-1500-20260917T150000.opus",
        "2026-09-17T15:00:00+00:00",
        &a_result("this session was deleted after it was transcribed"),
    );
    meaning
        .execute(
            "INSERT INTO deleted_segments (source_id, start_utc, deleted_utc)
             VALUES ('meeting-20260917-1500', '2026-09-17T15:00:00+00:00', '2026-09-17T16:00:00+00:00')",
            [],
        )
        .expect("tombstone");

    let pass = write_pass(&mut meaning, &ingest, &PER_MIC, NOW, 20).expect("pass");
    assert_eq!(pass.barren, 1);
    let outcome: String = ingest
        .query_row("SELECT outcome FROM pass_ledger", [], |r| r.get(0))
        .expect("one ledger row");
    assert_eq!(outcome, "deleted");
    assert_eq!(
        write_pass(&mut meaning, &ingest, &PER_MIC, NOW, 20).expect("again"),
        Pass::default(),
        "a deleted session is decided once, not re-examined on every pass"
    );
}

#[test]
fn a_limited_pass_takes_the_oldest_clip_across_sources_first() {
    // Filename order is source order: every clip of `aaa` would drain before
    // the older clips of `zzz`.
    let (mut meaning, ingest, _dir) = planes_for_a_pass();
    for (source, start) in [
        ("aaa", "2026-09-11T11:00:00+00:00"),
        ("zzz", "2026-09-11T10:00:00+00:00"),
    ] {
        let stamp = start[..19].replace(['-', ':'], "");
        done_job(
            &ingest,
            PER_MIC.kind,
            source,
            &format!("{source}-{stamp}.flac"),
            start,
            &a_result("ik denk dat we dat morgen moeten doen"),
        );
        meaning
            .execute(
                "INSERT INTO audio_segments
                     (source_id, path, start_utc, end_utc, sample_rate, channels)
                 VALUES (?1, '/x', ?2, ?3, 16000, 1)",
                (source, start, &minute_later(start)),
            )
            .expect("clip");
    }

    let pass = write_pass(&mut meaning, &ingest, &PER_MIC, NOW, 1).expect("pass");
    assert_eq!(pass.turns, 1);
    let written: String = meaning
        .query_row("SELECT start_utc FROM transcript_segments", [], |r| {
            r.get(0)
        })
        .expect("turn");
    assert!(written.starts_with("2026-09-11T10:00"), "{written}");
}
