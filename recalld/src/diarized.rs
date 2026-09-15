//! Stage E4's write: replacing a block's machine turns with speaker-aligned ones.
//!
//! ⚠ **This is the most destructive pass in the system, and the rules below are
//! the ones that were got wrong.** `refine.py` applied its filters AFTER hiding
//! the existing turns, so a pass whose every turn was filtered out — or which
//! produced none at all — hid the transcript and wrote nothing in its place. It
//! blanked 132 segments of real household conversation that way, including a
//! minute of Dutch about writing things down to remember them.
//!
//! So the whole decision is made HERE, on data, before anything is written:
//! [`decide`] takes what exists and what the pass produced and answers with a
//! [`Swap`] that either replaces or keeps. A pass replaces a transcript or it
//! keeps it. It never empties one.
//!
//! Ported from `refine._replace_turns`, rule for rule, because both run until
//! the Python retires and a divergence would be a bug rather than a variant.

use crate::align::AlignedTurn;
use crate::quality::is_repetition_loop;
use chrono::{DateTime, Duration, Utc};

/// Languages this household actually speaks. A whole-block detection outside
/// this set is the model hallucinating on unclear audio — the turns are kept
/// (they have audio behind them) but their confidence is zeroed rather than
/// asserted.
pub const HOUSEHOLD_LANGUAGES: [&str; 2] = ["nl", "en"];

/// Below this fraction of the existing visible text, a pass is declined.
///
/// A refined pass that comes back with far less text than the block already has
/// is a degenerate transcription — a truncated long-form decode, a whole-clip
/// mis-detection — and swapping it in would hide the good transcript from every
/// view.
pub const MIN_COVERAGE_RATIO: f64 = 0.5;

/// …and only once there is a substantial transcript to protect. Tiny blocks
/// swing too wildly in ratio for the bar to mean anything.
pub const COVERAGE_REF_MIN_CHARS: usize = 200;

/// A machine turn already standing on this block.
#[derive(Debug, Clone, PartialEq)]
pub struct Existing {
    pub id: i64,
    pub text: String,
}

/// A span a person has corrected, in absolute time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Corrected {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

/// Why a swap was declined. Each names the arithmetic, because a refusal nobody
/// can read is indistinguishable from a bug — and these refusals are recorded
/// against clips that then wait for a fixed pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The pass produced no turns at all: no words, or no speaker spans.
    NothingAligned,
    /// It produced turns and every one was a repetition loop or inside a span a
    /// person has already corrected.
    AllFiltered { produced: usize },
    /// It produced far less text than the block already has.
    Coverage { existing: usize, new: usize },
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NothingAligned => write!(f, "nothing-aligned"),
            Self::AllFiltered { produced } => write!(
                f,
                "all-turns-filtered: the pass produced {produced} turn(s), every one a \
                 repetition loop or inside a human-corrected span"
            ),
            Self::Coverage { existing, new } => write!(
                f,
                "coverage-guard: new {new} chars < {:.0}% of existing {existing}",
                MIN_COVERAGE_RATIO * 100.0
            ),
        }
    }
}

/// What a diarized pass would do to one block.
#[derive(Debug, Clone, PartialEq)]
pub enum Swap {
    /// Hide `hide` and write `insert`, in ONE transaction — a crash between them
    /// would leave the block blank and, because the marker is what keeps it from
    /// being re-picked, never re-derived.
    Replace {
        insert: Vec<AlignedTurn>,
        hide: Vec<i64>,
    },
    /// Keep what is there. The existing transcript is untouched.
    Keep(Refusal),
}

fn overlaps(a: (DateTime<Utc>, DateTime<Utc>), b: (DateTime<Utc>, DateTime<Utc>)) -> bool {
    a.0 < b.1 && a.1 > b.0
}

/// Seconds-from-block-start to an absolute instant.
fn at(block_start: DateTime<Utc>, offset_s: f64) -> DateTime<Utc> {
    block_start + Duration::milliseconds((offset_s * 1000.0).round() as i64)
}

/// Decide the swap for one block. Pure, so every rule here is testable without a
/// database — they are the rules that can destroy a person's words.
///
/// 1. **A turn inside a human-corrected span is DROPPED**, and so is a
///    repetition loop. Both before anything else is considered.
/// 2. **If the block has turns and nothing survives the filter, KEEP.** This is
///    the 132-segment rule. Note the `existing` condition: a block with no turns
///    at all and nothing to write is not a refusal, it is a block with nothing
///    in it.
/// 3. **If the surviving text is far smaller than what is there, KEEP.**
///    ⚠ BOTH sides are filtered the same way, and the symmetry is the point.
///    Counting the existing side RAW let a hallucination win by length: a
///    Whisper loop is hundreds of characters of nothing, so every honest pass
///    measured as "covering too little", the loop was kept, and the block was
///    marked skipped — garbage preserved, never retried. Measured on the archive
///    2026-09-02: 10 of 94 guard-skipped segments were held that way.
#[must_use]
pub fn decide(
    block_start: DateTime<Utc>,
    aligned: Vec<AlignedTurn>,
    existing: &[Existing],
    human: &[Corrected],
) -> Swap {
    if aligned.is_empty() {
        return Swap::Keep(Refusal::NothingAligned);
    }
    let produced = aligned.len();
    let keep: Vec<AlignedTurn> = aligned
        .into_iter()
        .filter(|t| !is_repetition_loop(&t.text))
        .filter(|t| {
            let span = (at(block_start, t.start), at(block_start, t.end));
            !human.iter().any(|c| overlaps(span, (c.start, c.end)))
        })
        .collect();
    if !existing.is_empty() && keep.is_empty() {
        return Swap::Keep(Refusal::AllFiltered { produced });
    }
    let existing_chars: usize = existing
        .iter()
        .filter(|o| !is_repetition_loop(&o.text))
        .map(|o| o.text.chars().count())
        .sum();
    let new_chars: usize = keep.iter().map(|t| t.text.chars().count()).sum();
    #[expect(
        clippy::cast_precision_loss,
        reason = "a block's character count is far inside f64's exact integer range"
    )]
    let too_little = existing_chars >= COVERAGE_REF_MIN_CHARS
        && (new_chars as f64) < MIN_COVERAGE_RATIO * (existing_chars as f64);
    if too_little {
        return Swap::Keep(Refusal::Coverage {
            existing: existing_chars,
            new: new_chars,
        });
    }
    Swap::Replace {
        insert: keep,
        hide: existing.iter().map(|o| o.id).collect(),
    }
}

/// Whether a whole-block language detection is one to trust a confidence from.
#[must_use]
pub fn reliable_language(language: Option<&str>) -> bool {
    language.is_some_and(|l| HOUSEHOLD_LANGUAGES.contains(&l))
}

// --- the write ---------------------------------------------------------------

use crate::align::{SpeakerTurn, Word, assign_words_to_speakers};
use chrono::SecondsFormat;
use serde::Deserialize;

/// `provenance` a diarized-aligned turn records — the reversal key.
///
/// ⚠ Spelled to match what `refine.py` has been writing for months
/// (`ALIGNED_MARKER (model)`), because `recalld::reads::tier()`,
/// `audio::render_blocking` and `assign` all test `provenance.starts_with`
/// against it. A new spelling would make every turn this writes read as
/// un-diarized to three separate readers.
#[must_use]
pub fn provenance(model: &str) -> String {
    format!("diarized-aligned ({model})")
}

/// …and what the turns it supersedes record in `hidden_reason`.
#[must_use]
pub fn hidden_by(model: &str) -> String {
    format!("diarized ({model})")
}

#[derive(Deserialize)]
struct Reply<T> {
    ok: bool,
    result: Option<T>,
}

#[derive(Deserialize)]
struct Voices {
    #[serde(default)]
    turns: Vec<SpeakerTurn>,
}

#[derive(Deserialize)]
struct Transcription {
    language: Option<String>,
    #[serde(default)]
    segments: Vec<TranscribedSegment>,
}

/// One word as `transcript_segments.word_timings` STORES it.
///
/// ⚠ **`{s, e, w}`, not `{start, end, text}`, and no probability at all.** This
/// is `store._dump_word_timings`'s shape, read back by `store._load_word_timings`
/// (which substitutes `probability=1.0`) and by the boundary editor. Deriving
/// `Serialize` on `align::Word` and writing that instead was the first attempt
/// here: it produces valid JSON in a shape NOTHING reads, so the words would
/// simply vanish from every turn this pass writes — silently, because a turn
/// with unparseable timings is indistinguishable from one with none.
#[derive(serde::Serialize)]
struct Stored {
    s: f64,
    e: f64,
    w: String,
}

#[derive(Deserialize)]
struct TranscribedSegment {
    #[serde(default)]
    words: Vec<Word>,
}

/// The speaker spans a stored `diarize-room` result carries.
///
/// # Errors
/// `None` when the shim refused or the body is not this shape — both permanent,
/// because a stored result does not change on a later pass.
#[must_use]
pub fn speaker_turns(stored: &str) -> Option<Vec<SpeakerTurn>> {
    let reply: Reply<Voices> = serde_json::from_str(stored).ok()?;
    reply.ok.then_some(reply.result?.turns)
}

/// Every word a stored `transcribe-room` result carries, in order, with the
/// block's detected language.
///
/// ⚠ **Words, not segments.** Alignment assigns each WORD to whoever was
/// speaking at its midpoint; a segment-level assignment would put a whole
/// sentence on one speaker and is the coarse behaviour stage E4 exists to
/// replace. A result with no word timings therefore yields nothing, and the
/// caller keeps the transcript it has.
#[must_use]
pub fn words_of(stored: &str) -> Option<(Vec<Word>, Option<String>)> {
    let reply: Reply<Transcription> = serde_json::from_str(stored).ok()?;
    if !reply.ok {
        return None;
    }
    let outcome = reply.result?;
    let words: Vec<Word> = outcome
        .segments
        .into_iter()
        .flat_map(|s| s.words)
        .filter(|w| w.end > w.start)
        .collect();
    (!words.is_empty()).then_some((words, outcome.language))
}

/// Apply a [`Swap::Replace`] to one block. ONE transaction: the hides, the
/// inserts and their search-index rows land together or not at all.
///
/// ⚠ **A crash between the hide and the inserts would leave the block BLANK and
/// never re-derived** — the provenance marker is what keeps it from being picked
/// again, so the half-applied state is indistinguishable from a finished one.
/// That is why this is not two calls.
///
/// # Errors
/// If the transaction cannot be taken or any statement fails. Nothing is left
/// half-applied.
pub fn apply(
    conn: &mut rusqlite::Connection,
    audio_segment_id: i64,
    block_start: DateTime<Utc>,
    swap: &Swap,
    language: Option<&str>,
    model: &str,
    now: &str,
) -> rusqlite::Result<usize> {
    let Swap::Replace { insert, hide } = swap else {
        return Ok(0);
    };
    if insert.is_empty() {
        return Ok(0);
    }
    let trusted = reliable_language(language);
    let tx = conn.transaction()?;
    for id in hide {
        tx.execute(
            "UPDATE transcript_segments SET hidden_reason = ?1
             WHERE id = ?2 AND hidden_reason IS NULL",
            (hidden_by(model), id),
        )?;
    }
    let mut written = 0;
    for turn in insert {
        // Word timings are re-based to the TURN's start, so a later boundary
        // edit can snap to a real word time and play exactly that span.
        let rebased: Vec<Stored> = turn
            .words
            .iter()
            .map(|w| Stored {
                s: w.start - turn.start,
                e: w.end - turn.start,
                w: w.text.clone(),
            })
            .collect();
        tx.execute(
            "INSERT INTO transcript_segments
                 (audio_segment_id, start_utc, end_utc, text, language,
                  asr_confidence, asr_model, speaker_cluster, provenance,
                  word_timings, created_utc)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                audio_segment_id,
                at(block_start, turn.start).to_rfc3339_opts(SecondsFormat::Micros, false),
                at(block_start, turn.end).to_rfc3339_opts(SecondsFormat::Micros, false),
                turn.text,
                language,
                // A non-household language for the whole block is the model
                // hallucinating on unclear audio: keep the turn, assert no
                // confidence in it.
                if trusted { turn.confidence } else { 0.0 },
                model,
                turn.speaker,
                provenance(model),
                serde_json::to_string(&rebased).unwrap_or_else(|_| "[]".to_owned()),
                now,
            ],
        )?;
        let id = tx.last_insert_rowid();
        // ⚠ Maintained in CODE — `transcript_fts` is contentless FTS5. Forgetting
        // it fails nothing; it just makes the text unfindable by the one route
        // most likely to look for it.
        tx.execute(
            "INSERT INTO transcript_fts (rowid, text) VALUES (?1, ?2)",
            (id, &turn.text),
        )?;
        written += 1;
    }
    tx.commit()?;
    Ok(written)
}

/// The machine turns standing on a block, and the spans a person has corrected
/// inside it — the two things [`decide`] needs from the database.
///
/// ⚠ A HUMAN turn is not "existing" for this purpose: it is never superseded and
/// never hidden, and including it would let the coverage guard measure a
/// person's own words as something to be replaced.
///
/// # Errors
/// If the database refuses.
pub fn standing(
    conn: &rusqlite::Connection,
    audio_segment_id: i64,
) -> rusqlite::Result<Vec<Existing>> {
    let mut stmt = conn.prepare(
        "SELECT id, text FROM transcript_segments
         WHERE audio_segment_id = ?1 AND superseded_by IS NULL
           AND hidden_reason IS NULL AND asr_model <> 'human'",
    )?;
    let rows = stmt.query_map([audio_segment_id], |r| {
        Ok(Existing {
            id: r.get(0)?,
            text: r.get(1)?,
        })
    })?;
    rows.collect()
}

/// Every corrected span overlapping `[from, to)`.
///
/// # Errors
/// If the database refuses. A stored instant that will not parse is SKIPPED
/// rather than defaulted — a corrected span placed at the epoch would protect
/// nothing and silently let a machine pass overwrite a person's words.
pub fn corrections(
    conn: &rusqlite::Connection,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> rusqlite::Result<Vec<Corrected>> {
    let mut stmt = conn.prepare(
        "SELECT start_utc, end_utc FROM corrections
         WHERE start_utc < ?2 AND end_utc > ?1",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![
            from.to_rfc3339_opts(SecondsFormat::Micros, false),
            to.to_rfc3339_opts(SecondsFormat::Micros, false)
        ],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
    )?;
    let mut out = Vec::new();
    for row in rows {
        let (start, end) = row?;
        if let (Ok(start), Ok(end)) = (
            DateTime::parse_from_rfc3339(&start),
            DateTime::parse_from_rfc3339(&end),
        ) {
            out.push(Corrected {
                start: start.with_timezone(&Utc),
                end: end.with_timezone(&Utc),
            });
        }
    }
    Ok(out)
}

// --- the pass ----------------------------------------------------------------

/// What one diarized pass did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pass {
    pub blocks: usize,
    pub turns: usize,
    pub hidden: usize,
    /// Blocks whose existing transcript was KEPT, by refusal. The number worth
    /// watching: a pass that keeps most of what it looks at is reporting on the
    /// audio or on the guards, not doing work.
    pub kept: usize,
    /// Blocks waiting on something transient — no audio segment registered yet,
    /// or no words to align against. These get NO ledger row.
    pub waiting: usize,
}

/// Drain the finished `diarize-room` jobs into speaker-aligned turns.
///
/// ⚠ **This pass REPLACES turns, which no other pass does.** `turns::write_pass`
/// only ever writes where there is nothing; this hides what is there and writes
/// over it, so every refusal goes through [`decide`] first and every terminal
/// decision leaves a ledger row. A decision that writes no row is a decision
/// made again for ever.
///
/// ⚠ The two transient outcomes get NO row, deliberately: a clip whose audio
/// segment is not yet registered, and one whose transcription carried no word
/// timings that this pass can see. Both can become eligible later, and a row
/// would retire them permanently for being examined too early.
///
/// # Errors
/// If either database refuses.
pub fn write_pass(
    meaning: &mut rusqlite::Connection,
    ingest: &rusqlite::Connection,
    model: &str,
    now: &str,
    limit: usize,
) -> rusqlite::Result<Pass> {
    crate::turns::ensure_ledger(ingest)?;
    // The diarize job and the transcription it aligns against, joined on the
    // filename they share — the words and the speaker spans are two results
    // about ONE clip, and reading them separately is how they get out of step.
    let mut stmt = ingest.prepare(
        "SELECT d.filename, d.result, t.result, s.source
         FROM jobs d
         JOIN jobs t ON t.filename = d.filename AND t.kind = ?2
                    AND t.done_utc IS NOT NULL AND t.result IS NOT NULL
         JOIN segments s ON s.filename = d.filename
         WHERE d.kind = ?1 AND d.done_utc IS NOT NULL AND d.result IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM pass_ledger l
                           WHERE l.kind = ?1 AND l.filename = d.filename)
         ORDER BY d.filename ASC",
    )?;
    let jobs: Vec<(String, String, String, String)> = stmt
        .query_map(
            rusqlite::params![crate::queue::DIARIZE_ROOM, crate::queue::TRANSCRIBE_ROOM],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?
        .collect::<Result<_, _>>()?;

    let kind = crate::queue::DIARIZE_ROOM;
    let mut pass = Pass::default();
    for (filename, voices, transcription, source) in jobs {
        if pass.blocks >= limit {
            break;
        }
        let Some(block_start) = audiocore::names::parse_segment_start(&filename) else {
            crate::turns::ledger(ingest, kind, &filename, "unnameable", now)?;
            continue;
        };
        let Some(speakers) = speaker_turns(&voices) else {
            // The shim refused, or sent a shape this does not understand. Both
            // permanent: a stored result does not change on a later pass.
            crate::turns::ledger(ingest, kind, &filename, "unreadable", now)?;
            pass.kept += 1;
            continue;
        };
        let Ok((audio_id, end_raw)) = meaning.query_row(
            "SELECT id, end_utc FROM audio_segments
             WHERE source_id = ?1 AND start_utc LIKE ?2",
            rusqlite::params![
                source,
                format!("{}%", block_start.format("%Y-%m-%dT%H:%M:%S"))
            ],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        ) else {
            pass.waiting += 1;
            continue;
        };
        // ⚠ Transient, so no ledger row: a transcription without word timings
        // today may be re-derived with them.
        let Some((words, language)) = words_of(&transcription) else {
            pass.waiting += 1;
            continue;
        };
        // An unparseable end collapses the window to the block's start, so the
        // correction lookup finds nothing and the pass refuses rather than
        // writing over a span it could not check.
        let block_end = DateTime::parse_from_rfc3339(&end_raw)
            .map_or_else(|_| at(block_start, 0.0), |t| t.with_timezone(&Utc));
        let existing = standing(meaning, audio_id)?;
        let human = corrections(meaning, block_start, block_end)?;
        let aligned = assign_words_to_speakers(&words, &speakers, crate::align::MIN_TURN_S);
        let swap = decide(block_start, aligned, &existing, &human);
        pass.blocks += 1;
        match &swap {
            Swap::Keep(why) => {
                crate::turns::ledger(ingest, kind, &filename, &why.to_string(), now)?;
                pass.kept += 1;
            }
            Swap::Replace { hide, .. } => {
                let written = apply(
                    meaning,
                    audio_id,
                    block_start,
                    &swap,
                    language.as_deref(),
                    model,
                    now,
                )?;
                pass.turns += written;
                pass.hidden += hide.len();
                crate::turns::ledger(ingest, kind, &filename, "aligned", now)?;
            }
        }
    }
    Ok(pass)
}
