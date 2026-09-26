//! Turns finished transcription jobs into transcript rows.
//!
//! The runner retires each job with the shim's reply as opaque JSON
//! (`queue::done`); this reads it.
//!
//! One stream: each microphone's own clip (`transcribe-segment`). A pass fills
//! clips that have no turns and hides nothing but the live guesses it replaces.
//!
//! Every row a pass writes is deletable by `provenance = 'per-mic (runner)'`
//! and by nothing else; a NULL provenance could not be taken back.
//!
//! Hiding is not safer than deleting: a hidden row is still in `transcript_fts`,
//! still counted, and still seen by supersession.

use crate::ledger::{Outcome, PassKind, record};
use crate::turn_store::{self, NewTurn, Protected, Provenance};
use audiocore::instant;
use audiocore::instant::Stamp;
use audiocore::job::Kind;
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;

/// One turn a clip's transcript implies, in the archive's own terms.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipTurn {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub text: String,
    pub language: Option<String>,
    pub confidence: Option<f64>,
    /// The shim's per-word timings, verbatim, or `None` when it sent none.
    pub word_timings: Option<String>,
}

/// Why a stored result yields no turns. All are ordinary outcomes, not faults.
#[derive(Debug, PartialEq)]
pub enum Barren {
    /// The shim reported failure (`ok: false`). The clip is the problem.
    Refused,
    /// Valid JSON, no segments — a clip with nothing said in it.
    NothingSaid,
    /// The stored result is not the shape this understands.
    Unreadable(String),
}

#[derive(Deserialize)]
struct Reply {
    ok: bool,
    result: Option<Transcription>,
}

#[derive(Deserialize)]
struct Transcription {
    language: Option<String>,
    #[serde(default)]
    segments: Vec<Segment>,
}

#[derive(Deserialize)]
struct Segment {
    start: f64,
    end: f64,
    text: String,
    confidence: Option<f64>,
    #[serde(default)]
    words: Option<serde_json::Value>,
}

/// Seconds-from-clip-start to an absolute instant.
fn at(block_start: DateTime<Utc>, offset_s: f64) -> DateTime<Utc> {
    block_start + Duration::milliseconds((offset_s * 1000.0).round() as i64)
}

/// Interpret one stored job result as the turns it implies.
///
/// `block_start` comes from the clip's filename; the shim's offsets are relative
/// to the clip and mean nothing on their own.
///
/// A segment with no alphanumeric character, or with no duration, is dropped
/// here: near-silence comes back as invented text (e.g. "Thank you." or a run of
/// tildes), not as nothing.
pub fn interpret(block_start: DateTime<Utc>, stored: &str) -> Result<Vec<ClipTurn>, Barren> {
    let reply: Reply =
        serde_json::from_str(stored).map_err(|e| Barren::Unreadable(e.to_string()))?;
    if !reply.ok {
        return Err(Barren::Refused);
    }
    let outcome = reply.result.ok_or(Barren::NothingSaid)?;
    let turns: Vec<ClipTurn> = outcome
        .segments
        .into_iter()
        .filter(|s| s.text.chars().any(char::is_alphanumeric))
        .filter(|s| s.end > s.start)
        .map(|s| ClipTurn {
            start: at(block_start, s.start),
            end: at(block_start, s.end),
            text: s.text.trim().to_owned(),
            language: outcome.language.clone(),
            confidence: s.confidence,
            word_timings: s.words.as_ref().map(std::string::ToString::to_string),
        })
        .collect();
    if turns.is_empty() {
        return Err(Barren::NothingSaid);
    }
    Ok(turns)
}

/// What a write would do, decided before anything is written.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    pub insert: Vec<ClipTurn>,
    /// Turns declined, and why, so a refusal is visible rather than silent.
    pub refused: Vec<String>,
    /// Turns swept by the quality rules (loops, wordless text, words invented
    /// over near-silence). Apart from
    /// `refused`: a refusal means a person's words are in the way, a sweep
    /// means the model failed.
    pub swept: usize,
}

fn overlaps(a: (DateTime<Utc>, DateTime<Utc>), b: (DateTime<Utc>, DateTime<Utc>)) -> bool {
    a.0 < b.1 && a.1 > b.0
}

/// Decide the write for one clip. Pure, so the rules that protect a person's
/// typed words are testable without a database.
///
/// 1. A repetition loop or wordless turn is swept, and so is what the model
///    writes over silence when the clip's measured `speech` is near-silent.
/// 2. A turn overlapping a person-owned span is refused: the person's text stands.
#[must_use]
pub fn plan(turns: Vec<ClipTurn>, human: &[Protected], speech: Option<f64>) -> Plan {
    let mut out = Plan::default();
    for turn in turns {
        if crate::quality::is_repetition_loop(&turn.text)
            || crate::quality::is_wordless(&turn.text)
            || crate::quality::is_invented_over_silence(&turn.text, speech)
        {
            out.swept += 1;
            continue;
        }
        if human
            .iter()
            .any(|c| overlaps((turn.start, turn.end), (c.start, c.end)))
        {
            out.refused.push(format!(
                "human-corrected span {}..{} — the person's text stands",
                turn.start.to_rfc3339(),
                turn.end.to_rfc3339()
            ));
            continue;
        }
        out.insert.push(turn);
    }
    out
}

/// What one registrar pass did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Registered {
    /// Clips given an `audio_segments` row, and therefore somewhere to hang a turn.
    pub added: usize,
    /// Clips whose source the meaning plane does not know yet. The only
    /// non-terminal outcome: everything else is ledgered and not revisited.
    pub waiting: usize,
    /// Clips with an unparseable name or that ffmpeg could not read. Ledgered.
    pub unreadable: usize,
    /// Clips whose minute a sibling file (e.g. the `.wav` beside the `.opus`)
    /// already holds under `UNIQUE (source_id, start_utc)`. Common, not a fault;
    /// counted apart from `added` so the duplicates stay visible.
    pub covered: usize,
    /// Clips already registered by name, retired cheaply.
    pub retired: usize,
    /// Clips whose file was decoded: the pass's whole cost. Lets a test assert
    /// that a duplicate costs no decode.
    pub probed: usize,
}

/// Register microphone clips from the ingest plane in the meaning plane, so
/// their turns have audio to hang on.
///
/// The path is the ingest copy (`<root>/ingest/<source>/`), the complete one.
/// `INSERT OR IGNORE` on `UNIQUE (source_id, start_utc)` means an already
/// registered clip keeps its path.
///
/// `end_utc` is decoded, not assumed: a microphone clip is whatever the segment
/// muxer closed, and `write_pass` sizes the human-correction window from it.
///
/// A clip whose source is unknown waits: `sources.kind` is the sender's to
/// declare, and registering under a guess is permanent.
///
/// `limit` bounds probes, because a probe decodes the whole file. Cheap terminal
/// decisions are not charged against it, so a backlog of them drains in one pass.
///
/// ⚠ Every terminal outcome must be ledgered. The candidate query is "not in the
/// ledger", so an unledgered decision is re-made, and possibly re-decoded, on
/// every pass.
///
/// # Errors
/// If either database refuses.
pub fn register_segments(
    meaning: &rusqlite::Connection,
    ingest: &rusqlite::Connection,
    root: &std::path::Path,
    now: &Stamp,
    limit: usize,
) -> rusqlite::Result<Registered> {
    // Uploads are excluded because `upload::register` writes their meaning-plane
    // rows; they are transcribed like any microphone clip.
    let mics: std::collections::HashSet<String> = {
        let mut stmt =
            meaning.prepare("SELECT id FROM sources WHERE kind NOT IN ('upload', ?1)")?;
        let rows = stmt.query_map([crate::store::ROOM_KIND], |r| r.get::<_, String>(0))?;
        rows.collect::<Result<_, _>>()?
    };
    // Read once into sets rather than looked up per candidate; see
    // `registered_names`.
    let have = registered_names(meaning)?;
    let mut minutes = registered_minutes(meaning)?;

    // Newest first, unlike `write_pass`: a clip arriving now must not wait
    // behind a backfill, and the ledger retires what cannot be read, so nothing
    // starves.
    let candidates: Vec<(String, String)> = {
        let mut stmt = ingest.prepare(
            "SELECT s.filename, s.source FROM segments s
             WHERE s.source != ?1
               AND NOT EXISTS (SELECT 1 FROM pass_ledger l
                               WHERE l.kind = ?2 AND l.filename = s.filename)
             ORDER BY s.start_utc DESC",
        )?;
        let rows = stmt.query_map((crate::store::ROOM_SOURCE, PassKind::Register), |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        rows.collect::<Result<_, _>>()?
    };

    let mut out = Registered::default();
    let mut probes = 0;
    for (filename, source) in candidates {
        // The budget bounds decodes only; cheap decisions below do not spend it.
        if probes >= limit {
            break;
        }
        let decided = |outcome| record(ingest, PassKind::Register, &filename, outcome, None, now);
        // Already registered. Terminal, so ledgered.
        if have.contains(&filename) {
            decided(Outcome::AlreadyRegistered)?;
            out.retired += 1;
            continue;
        }
        // ⚠ Not ledgered, unlike every other outcome: the source may be
        // registered later, and a ledgered clip would never come back.
        if !mics.contains(&source) {
            out.waiting += 1;
            continue;
        }
        let Some(start) = audiocore::names::parse_segment_start(&filename) else {
            out.unreadable += 1;
            decided(Outcome::Unnameable)?;
            continue;
        };
        // A sibling already holds this minute, so the insert could only be
        // ignored. Decided without decoding.
        let minute = (source.clone(), instant::python_isoformat_utc(start));
        if minutes.contains(&minute) {
            out.covered += 1;
            decided(Outcome::CoveredBySibling)?;
            continue;
        }
        let path = crate::store::source_dir(root, &source).join(&filename);
        probes += 1;
        out.probed += 1;
        let Ok(media) = crate::upload::probe(&path) else {
            // Permanent: e.g. a header-only file from a dead capture holds no
            // audio and never will.
            out.unreadable += 1;
            decided(Outcome::Unreadable)?;
            continue;
        };
        let end = start + Duration::microseconds((media.duration_s * 1e6).round() as i64);
        let inserted = meaning.execute(
            "INSERT OR IGNORE INTO audio_segments
                 (source_id, path, start_utc, end_utc, sample_rate, channels)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                source,
                path.to_string_lossy(),
                instant::python_isoformat_utc(start),
                instant::python_isoformat_utc(end),
                media.sample_rate,
                media.channels,
            ],
        )?;
        // An ignored insert means a sibling holds this minute: terminal, and
        // ledgered like a registration.
        if inserted == 1 {
            out.added += 1;
            // `minutes` is a snapshot from before the loop; without this, a
            // sibling later in the same pass would pay a full decode.
            minutes.insert(minute);
        } else {
            out.covered += 1;
        }
        let outcome = if inserted == 1 {
            Outcome::Registered
        } else {
            Outcome::CoveredBySibling
        };
        decided(outcome)?;
    }
    Ok(out)
}

/// Every registered clip's basename. One pass over the column, because a
/// correlated lookup per candidate scans both tables and takes minutes.
fn registered_names(
    meaning: &rusqlite::Connection,
) -> rusqlite::Result<std::collections::HashSet<String>> {
    let mut stmt = meaning.prepare("SELECT path FROM audio_segments")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut set = std::collections::HashSet::new();
    for path in rows {
        if let Some(name) = path?.rsplit('/').next() {
            set.insert(name.to_owned());
        }
    }
    Ok(set)
}

/// Every registered `(source_id, start_utc)`: the key the `UNIQUE` constraint
/// rejects on. A sibling file (the `.wav` beside the `.opus`) misses
/// [`registered_names`], but its start time is in its name, so this catches it
/// before a decode.
fn registered_minutes(
    meaning: &rusqlite::Connection,
) -> rusqlite::Result<std::collections::HashSet<(String, String)>> {
    let mut stmt = meaning.prepare("SELECT source_id, start_utc FROM audio_segments")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    rows.collect()
}

/// Apply a [`Plan`] to one clip in one transaction: the turns, their search
/// rows and the live guesses they replace land together or not at all.
///
/// Idempotent by refusing: a clip whose audio segment already carries turns is
/// left alone and the caller gets `Ok(0)`, so a second pass neither duplicates
/// turns nor overwrites an edited minute.
///
/// # Errors
/// If the transaction cannot be taken or any statement fails. Nothing is left
/// half-applied.
pub fn write_block(
    conn: &mut rusqlite::Connection,
    audio_segment_id: i64,
    span: (DateTime<Utc>, DateTime<Utc>),
    plan: &Plan,
    now: &Stamp,
) -> rusqlite::Result<usize> {
    if plan.insert.is_empty() {
        return Ok(0);
    }
    let tx = conn.transaction()?;
    let already: i64 = tx.query_row(
        "SELECT count(*) FROM transcript_segments WHERE audio_segment_id = ?1",
        [audio_segment_id],
        |row| row.get(0),
    )?;
    if already > 0 {
        return Ok(0);
    }
    let mut written = 0;
    for turn in &plan.insert {
        let (start, end) = (Stamp::of(turn.start), Stamp::of(turn.end));
        // Implausibly slow speech zeroes the confidence, as in `diarized`;
        // `word_spans` reads both timing encodings.
        let confidence = match turn.word_timings.as_deref() {
            Some(timings)
                if crate::quality::is_implausibly_slow(&crate::quality::word_spans(timings)) =>
            {
                Some(0.0)
            }
            _ => turn.confidence,
        };
        turn_store::insert(
            &tx,
            &NewTurn {
                audio_segment_id: Some(audio_segment_id),
                language: turn.language.as_deref(),
                asr_confidence: confidence,
                asr_model: Some(SHIM_MODEL),
                provenance: Some(Provenance::PerMic),
                word_timings: turn.word_timings.as_deref(),
                created_utc: Some(now),
                ..NewTurn::at(&start, &end, &turn.text)
            },
        )?;
        written += 1;
    }
    // The archive turn supersedes the live guess; without this the timeline
    // shows both.
    let (from, to) = span;
    turn_store::reconcile_live(
        &tx,
        &instant::python_isoformat_utc(from),
        &instant::python_isoformat_utc(to),
    )?;
    tx.commit()?;
    Ok(written)
}

/// What the `asr` shim loads when the caller names no model:
/// `recall.asr.DEFAULT_MODEL`.
///
/// ⚠ The queue carries no model field, so this copy must match the Python one.
/// The test `a_per_mic_turn_names_the_model_the_shim_will_actually_load` reads
/// `asr.py` and fails on a mismatch.
pub const SHIM_MODEL: &str = "mlx-community/whisper-large-v3-turbo";

/// The job kind a pass drains.
pub const KIND: Kind = Kind::TranscribeSegment;

/// What one pass did, for the log line.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pass {
    pub blocks: usize,
    pub turns: usize,
    pub refused: usize,
    pub barren: usize,
    /// Turns the quality rules swept (see [`plan`] rule 1).
    pub swept: usize,
}

/// Whether the block starting at `block_start` on `source` was deliberately
/// deleted, matched on the same timestamp spelling the audio lookup uses.
///
/// # Errors
/// If the meaning plane refuses.
pub fn tombstoned_block(
    meaning: &rusqlite::Connection,
    source: &str,
    block_start: DateTime<Utc>,
) -> rusqlite::Result<bool> {
    use rusqlite::OptionalExtension;
    Ok(meaning
        .query_row(
            "SELECT 1 FROM deleted_segments WHERE source_id = ?1 AND start_utc = ?2",
            rusqlite::params![source, instant::python_isoformat_utc(block_start)],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some())
}

/// Turn stored job results into visible turns, one clip at a time:
/// [`interpret`] → [`plan`] → [`write_block`].
///
/// ⚠ `limit` counts clips decided, and the SQL has no `LIMIT`: a limited query
/// returns the same ineligible rows every pass and never advances.
///
/// Ascending, so a backfill drains forward from the oldest undecided clip.
///
/// # Errors
/// If either database refuses. An uninterpretable result is counted and
/// skipped — one bad clip must not stop the queue draining.
pub fn write_pass(
    meaning: &mut rusqlite::Connection,
    ingest: &rusqlite::Connection,
    now: &Stamp,
    limit: usize,
) -> rusqlite::Result<Pass> {
    // ⚠ The source is joined from the ingest plane, never parsed from the
    // filename: `meeting-20260907-0905` is a source id, and no split of a
    // filename on a hyphen is safe.
    let mut stmt = ingest.prepare(
        "SELECT j.filename, j.result, s.source FROM jobs j
         JOIN segments s ON s.filename = j.filename
         WHERE j.kind = ?1 AND j.done_utc IS NOT NULL AND j.result IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM pass_ledger l
                           WHERE l.kind = ?1 AND l.filename = j.filename)
         ORDER BY s.start_utc ASC, j.filename ASC",
    )?;
    let jobs: Vec<(String, String, String)> = stmt
        .query_map(rusqlite::params![KIND], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<Result<_, _>>()?;

    let mut pass = Pass::default();
    for (filename, result, source) in jobs {
        if pass.blocks >= limit {
            break;
        }
        let Some(block_start) = audiocore::names::parse_segment_start(&filename) else {
            // Permanent: a name that is not a segment name never becomes one.
            pass.barren += 1;
            record(ingest, KIND, &filename, Outcome::Unnameable, None, now)?;
            continue;
        };
        // The clip's audio segment. Absent means it is not registered yet: wait,
        // never write a turn that cannot be played.
        //
        // ⚠ Not ledgered: this is the one transient barren cause, and a row
        // would retire the clip for being examined too early.
        let Ok((audio_id, end_raw)) = meaning.query_row(
            "SELECT id, end_utc FROM audio_segments
             WHERE source_id = ?1 AND start_utc = ?2",
            rusqlite::params![source, instant::python_isoformat_utc(block_start)],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        ) else {
            pass.barren += 1;
            // Unless the block was deleted: then the audio is never coming.
            if tombstoned_block(meaning, &source, block_start)? {
                record(ingest, KIND, &filename, Outcome::Deleted, None, now)?;
            }
            continue;
        };
        // Already written. Derived rather than ledgered, so deleting the turns
        // makes the clip eligible again.
        let written_already: i64 = meaning.query_row(
            "SELECT count(*) FROM transcript_segments WHERE audio_segment_id = ?1",
            [audio_id],
            |r| r.get(0),
        )?;
        if written_already > 0 {
            continue;
        }
        let Ok(turns) = interpret(block_start, &result) else {
            // Permanent: the stored result will not change.
            pass.barren += 1;
            record(ingest, KIND, &filename, Outcome::Unreadable, None, now)?;
            continue;
        };
        // ⚠ The end comes from the clip's own row: a clip can be short, and a
        // correction window that ends early cannot see a correction it is
        // about to overwrite.
        let Ok(block_end) = DateTime::parse_from_rfc3339(&end_raw) else {
            pass.barren += 1;
            record(ingest, KIND, &filename, Outcome::Unspanned, None, now)?;
            continue;
        };
        let block_end = block_end.with_timezone(&Utc);
        let human = turn_store::protected_between(meaning, block_start, block_end)?;
        let speech = crate::speech::seconds_of(ingest, &filename)?;
        let decided = plan(turns, &human, speech);
        pass.refused += decided.refused.len();
        pass.swept += decided.swept;
        let written = write_block(meaning, audio_id, (block_start, block_end), &decided, now)?;
        pass.turns += written;
        pass.blocks += 1;
        if written == 0 {
            // Decided but left no trace in the meaning plane; without this row
            // every later pass would reach it first.
            record(ingest, KIND, &filename, Outcome::NothingToWrite, None, now)?;
        }
    }
    Ok(pass)
}
