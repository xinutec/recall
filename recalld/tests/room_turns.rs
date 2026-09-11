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

// ---- the write plan: the rules that can destroy a person's typed words ----

use recalld::room_turns::{Corrected, Standing, plan};

fn t(s: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(s)
        .expect("time")
        .with_timezone(&chrono::Utc)
}

fn room_turn(start: &str, end: &str, text: &str) -> recalld::room_turns::RoomTurn {
    recalld::room_turns::RoomTurn {
        start: t(start),
        end: t(end),
        text: text.to_owned(),
        language: Some("nl".to_owned()),
        confidence: Some(0.9),
        word_timings: None,
    }
}

const A: &str = "2026-09-11T10:00:00Z";
const B: &str = "2026-09-11T10:00:10Z";
const C: &str = "2026-09-11T10:00:20Z";
const D: &str = "2026-09-11T10:00:30Z";

#[test]
fn a_room_turn_over_a_corrected_span_is_refused_with_a_reason() {
    // The human's text stands. A machine pass does not get to restate it.
    let out = plan(
        vec![room_turn(A, B, "what the model heard")],
        &[],
        &[Corrected {
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
    // Hiding is not deleting — but `hidden` is not `absent` either: the row
    // stays in transcript_fts, stays counted, stays visible to supersession.
    //
    // ⚠ The geometry here is deliberate, and the first draft of this test got it
    // wrong. Rule 2 is only REACHABLE when a standing turn overlaps a correction
    // while the room turn covering it does NOT — otherwise rule 1 refuses the
    // room turn first and rule 4 hides nothing, which is a different (also
    // correct) path. So:
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
        &[Corrected {
            start: t(A),
            end: t(B),
        }],
    );
    assert_eq!(out.insert.len(), 1, "the room turn misses the correction");
    assert_eq!(out.hide, vec![2], "the partly-corrected turn must survive");
}

#[test]
fn nothing_inserted_means_nothing_hidden() {
    // refine's lesson one stage later: applying the filters AFTER hiding blanked
    // 132 segments of real conversation. A pass replaces a transcript or keeps
    // it — it never empties one.
    let out = plan(
        vec![room_turn(A, B, "refused")],
        &[Standing {
            id: 1,
            start: t(A),
            end: t(B),
        }],
        &[Corrected {
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
fn an_uncovered_per_mic_turn_is_left_alone() {
    // Only what a WRITTEN room turn actually covers is hidden.
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
        &[Corrected {
            start: t(B),
            end: t(C),
        }],
    );
    assert_eq!(out.insert.len(), 1, "{:?}", out.refused);
}

// ---- registering built blocks in the meaning plane ----

use recalld::room_turns::{ROOM_CHANNELS, ROOM_RATE, register_blocks};

/// The two planes, as two connections — which is what they are in production.
fn two_planes() -> (rusqlite::Connection, rusqlite::Connection) {
    let meaning = rusqlite::Connection::open_in_memory().expect("meaning");
    meaning
        .execute_batch(
            "CREATE TABLE sources (id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL);
             CREATE TABLE audio_segments (
                 id INTEGER PRIMARY KEY,
                 source_id TEXT NOT NULL REFERENCES sources(id),
                 path TEXT NOT NULL, start_utc TEXT NOT NULL, end_utc TEXT NOT NULL,
                 sample_rate INTEGER NOT NULL, channels INTEGER NOT NULL,
                 UNIQUE (source_id, start_utc));",
        )
        .expect("meaning schema");
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
    // Exactly one minute: the builder works a UTC-aligned grid, so this is the one
    // duration that may be asserted rather than measured.
    assert!(end.starts_with("2026-09-11T10:01:00"), "{end}");
    assert_eq!(rate, ROOM_RATE);
    assert_eq!(channels, ROOM_CHANNELS);
}

#[test]
fn the_room_source_is_registered_as_derived_not_as_a_microphone() {
    // `deaf`, the liveness view and the sources panel all ask `is_device()`. A
    // derived stream registered as a device would be health-checked as a mic that
    // has no recorder, and would double-count the microphone it carried.
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
    // The microphones' own segments arrive by push from the Mac's archive. This
    // must never mint a second row for one of them.
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
