//! Replacing a block's machine turns with speaker-aligned ones.
//!
//! This is the most destructive pass in the system: it hides turns and writes
//! over them. The whole decision is made in [`decide`], on data, before anything
//! is written, so a filter that drops every new turn cannot leave the block
//! empty. A pass replaces a transcript, names it in place, or keeps it. It never
//! empties one.

use crate::align::AlignedTurn;
use crate::ledger::{Outcome, record};
use crate::quality::is_repetition_loop;
use crate::turn_store::{self, HiddenReason, NewTurn, Protected, Provenance};
use audiocore::instant::Stamp;
use audiocore::job::Kind;
use chrono::{DateTime, Duration, Utc};
use std::borrow::Cow;

/// Languages spoken in the archive. A whole-block detection outside this set is
/// the model hallucinating on unclear audio: the turns are kept but their
/// confidence is zeroed.
pub const HOUSEHOLD_LANGUAGES: [&str; 2] = ["nl", "en"];

/// Below this fraction of the existing visible text, a pass is declined: far
/// less text means a degenerate transcription (a truncated decode, a whole-clip
/// mis-detection), and swapping it in would hide the good transcript.
pub const MIN_COVERAGE_RATIO: f64 = 0.5;

/// The coverage bar applies only above this many existing characters: tiny
/// blocks swing too wildly in ratio for it to mean anything.
pub const COVERAGE_REF_MIN_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq)]
pub struct Existing {
    pub id: i64,
    pub text: String,
    /// The turn's own span, so attribution follows the diarization's evidence
    /// instead of assuming it covers the clip.
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    /// The stored `word_timings`, verbatim. Without them a turn can only be
    /// labelled, not divided between speakers.
    pub word_timings: Option<String>,
}

/// Why a swap was declined. Each names its arithmetic, because the refusal is
/// recorded against the clip and an unreadable one looks like a bug.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The pass produced no turns at all: no words, or no speaker spans.
    NothingAligned,
    /// Every turn was dropped, counted by the filter that dropped it. The two
    /// mean opposite things: a loop is the pass hallucinating, a corrected turn
    /// is the guard working.
    AllFiltered {
        loops: usize,
        corrected: usize,
    },
    Coverage {
        existing: usize,
        new: usize,
    },
    /// The pass would write fewer turns than exist, and none of its speaker
    /// spans overlaps an existing turn, so there is nothing to name either.
    Undiscriminating {
        produced: usize,
        existing: usize,
        speakers: usize,
    },
}

impl Refusal {
    /// How the ledger records it: the outcome, and its counts.
    pub fn recorded(&self) -> (Outcome, Option<serde_json::Value>) {
        match *self {
            Self::NothingAligned => (Outcome::NothingAligned, None),
            Self::AllFiltered { loops, corrected } => (
                Outcome::AllTurnsFiltered,
                Some(serde_json::json!({ "loops": loops, "corrected": corrected })),
            ),
            Self::Coverage { existing, new } => (
                Outcome::CoverageGuard,
                Some(serde_json::json!({ "new_chars": new, "existing_chars": existing })),
            ),
            Self::Undiscriminating {
                produced,
                existing,
                speakers,
            } => (
                Outcome::Undiscriminating,
                Some(serde_json::json!({
                    "produced": produced, "existing": existing, "speakers": speakers
                })),
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Swap {
    /// Hide `hide` and write `insert`, in one transaction (see [`apply`]).
    Replace {
        insert: Vec<AlignedTurn>,
        hide: Vec<i64>,
    },
    /// Name the turns that are already there, each with the speaker whose span
    /// covers it most. No text is rewritten and no boundary is lost.
    ///
    /// ⚠ This is the outcome whenever a pass would write FEWER turns than it
    /// hides, at any speaker count. A pass may split (more turns, every word
    /// kept) or label (no text touched); merging is neither, and is refused.
    Attribute {
        /// Existing turn id, and the speaker to name it.
        to: Vec<(i64, String)>,
    },
    Keep(Refusal),
}

/// One piece of a turn that carried two people's words.
#[derive(Debug, Clone, PartialEq)]
pub struct Piece {
    pub speaker: String,
    pub text: String,
    /// Seconds from the block's start, matching [`AlignedTurn`].
    pub start: f64,
    pub end: f64,
}

/// Divide `turn` where the speaker changes, keeping every word.
///
/// A split is the one rewrite permitted, because it loses nothing, so that is
/// checked per turn: if the pieces' words do not reconstruct the turn's own
/// text, this returns `None` and the caller labels the turn instead.
///
/// `None` also when there are no usable timings, or when every word belongs to
/// one speaker.
#[must_use]
pub fn split_at_speaker_changes(
    turn: &Existing,
    block_start: DateTime<Utc>,
    by: &[AlignedTurn],
) -> Option<Vec<Piece>> {
    let words = crate::quality::timed_words(turn.word_timings.as_deref()?);
    if words.is_empty() {
        return None;
    }
    // `by` is in seconds from the block's start: place each word there via the
    // turn's own start, or every word lands on the wrong speaker.
    let offset = (turn.start - block_start).as_seconds_f64();
    let base = words.first()?.start;

    let mut runs: Vec<(String, Vec<crate::quality::Word>)> = Vec::new();
    for word in words {
        // Where this word sits on the block's clock.
        let at = offset + (word.start - base);
        let speaker = by
            .iter()
            .find(|t| at >= t.start && at < t.end)
            .map(|t| t.speaker.clone())
            .or_else(|| nearest_speaker(at, by))?;
        match runs.last_mut() {
            Some((who, run)) if *who == speaker => run.push(word),
            _ => runs.push((speaker, vec![word])),
        }
    }
    if runs.len() < 2 {
        return None;
    }

    let pieces: Vec<Piece> = runs
        .iter()
        .map(|(speaker, run)| {
            let text = run
                .iter()
                .map(|w| w.text.trim())
                .collect::<Vec<_>>()
                .join(" ");
            Piece {
                speaker: speaker.clone(),
                text,
                start: offset + (run.first().map_or(0.0, |w| w.start) - base),
                end: offset + (run.last().map_or(0.0, |w| w.end) - base),
            }
        })
        .collect();

    // Compare on letters and digits only: the word list carries spacing and
    // punctuation differently from the turn's own text.
    let rebuilt = squashed(
        &pieces
            .iter()
            .map(|p| p.text.as_str())
            .collect::<Vec<_>>()
            .join(" "),
    );
    (rebuilt == squashed(&turn.text)).then_some(pieces)
}

/// Letters and digits, lowercased: what a split must preserve.
fn squashed(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// The speaker whose span is closest, for a word falling in a gap between them.
fn nearest_speaker(at: f64, by: &[AlignedTurn]) -> Option<String> {
    by.iter()
        .min_by(|a, b| {
            let da = (at - a.start).abs().min((at - a.end).abs());
            let db = (at - b.start).abs().min((at - b.end).abs());
            da.total_cmp(&db)
        })
        .map(|t| t.speaker.clone())
}

fn overlaps(a: (DateTime<Utc>, DateTime<Utc>), b: (DateTime<Utc>, DateTime<Utc>)) -> bool {
    a.0 < b.1 && a.1 > b.0
}

/// How many seconds `a` and `b` share, or `None` if they do not meet.
///
/// The shared span, not a boolean: a turn brushed by the end of one speaker's
/// span and covered by the next belongs to the second.
fn overlap_seconds(
    a: (DateTime<Utc>, DateTime<Utc>),
    b: (DateTime<Utc>, DateTime<Utc>),
) -> Option<f64> {
    let start = a.0.max(b.0);
    let end = a.1.min(b.1);
    (end > start).then(|| (end - start).as_seconds_f64())
}

/// Seconds-from-block-start to an absolute instant.
fn at(block_start: DateTime<Utc>, offset_s: f64) -> DateTime<Utc> {
    block_start + Duration::milliseconds((offset_s * 1000.0).round() as i64)
}

/// Decide the swap for one block. Pure, so every rule is testable without a
/// database.
///
/// 1. A repetition loop, or a turn inside a human-corrected span, is dropped.
/// 2. If the block has turns and nothing survives, keep. A block with no turns
///    and nothing to write is not a refusal.
/// 3. If fewer turns survive than exist, name the existing turns in place
///    ([`Swap::Attribute`]); keep if no span overlaps any of them.
/// 4. If the surviving text is far smaller than what is there, keep. Both sides
///    drop repetition loops, so a long hallucinated loop on the existing side
///    cannot make an honest pass look too short.
#[must_use]
pub fn decide(
    block_start: DateTime<Utc>,
    aligned: Vec<AlignedTurn>,
    existing: &[Existing],
    human: &[Protected],
) -> Swap {
    if aligned.is_empty() {
        return Swap::Keep(Refusal::NothingAligned);
    }
    let (mut loops, mut corrected) = (0, 0);
    let keep: Vec<AlignedTurn> = aligned
        .into_iter()
        .filter(|t| {
            if is_repetition_loop(&t.text) {
                loops += 1;
                return false;
            }
            let span = (at(block_start, t.start), at(block_start, t.end));
            if human.iter().any(|c| overlaps(span, (c.start, c.end))) {
                corrected += 1;
                return false;
            }
            true
        })
        .collect();
    if !existing.is_empty() && keep.is_empty() {
        return Swap::Keep(Refusal::AllFiltered { loops, corrected });
    }
    // ⚠ A pass must not flatten a finer transcript. Writing fewer turns than
    // exist loses boundaries at any speaker count, so it names in place instead.
    let speakers: std::collections::BTreeSet<&str> =
        keep.iter().map(|t| t.speaker.as_str()).collect();
    if keep.len() < existing.len() {
        // Each turn takes the speaker whose span covers most of it; a turn no
        // span touches is left unnamed rather than given a guess.
        let to: Vec<(i64, String)> = existing
            .iter()
            .filter_map(|o| {
                let best = keep
                    .iter()
                    .filter_map(|t| {
                        let span = (at(block_start, t.start), at(block_start, t.end));
                        overlap_seconds((o.start, o.end), span)
                            .map(|shared| (shared, t.speaker.as_str()))
                    })
                    .max_by(|a, b| a.0.total_cmp(&b.0))?;
                Some((o.id, best.1.to_owned()))
            })
            .collect();
        if !to.is_empty() {
            return Swap::Attribute { to };
        }
        return Swap::Keep(Refusal::Undiscriminating {
            produced: keep.len(),
            existing: existing.len(),
            speakers: speakers.len(),
        });
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
use audiocore::instant;
use serde::Deserialize;

#[derive(Deserialize)]
struct Reply<T> {
    ok: bool,
    result: Option<T>,
}

#[derive(Deserialize)]
struct Voices {
    #[serde(default)]
    turns: Vec<SpeakerTurn>,
    #[serde(default)]
    speakers: Vec<SpeakerVoice>,
}

/// One voiceprint the shim built for a speaker in this clip.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct SpeakerVoice {
    pub speaker: String,
    pub vector: Vec<f64>,
}

#[derive(Deserialize)]
struct Transcription {
    language: Option<String>,
    #[serde(default)]
    segments: Vec<TranscribedSegment>,
}

/// One word as `transcript_segments.word_timings` stores it.
///
/// ⚠ `{s, e, w}`, not `{start, end, text}`, and no probability. Serialising
/// `align::Word` instead gives valid JSON that no reader parses, and a turn with
/// unparseable timings looks the same as one with none.
#[derive(serde::Serialize)]
struct Stored {
    s: f64,
    e: f64,
    w: String,
}

#[derive(Deserialize)]
struct TranscribedSegment {
    /// The segment's own text, read only to judge whether the model looped on
    /// it. The turns are built from `words`.
    #[serde(default)]
    text: String,
    #[serde(default)]
    words: Vec<Word>,
}

/// The speaker spans a stored `diarize-segment` result carries.
///
/// # Errors
/// `None` when the shim refused or the body is not this shape. Both are
/// permanent: a stored result does not change.
#[must_use]
pub fn speaker_turns(stored: &str) -> Option<Vec<SpeakerTurn>> {
    Some(voices(stored)?.0)
}

/// The spans AND the per-speaker voiceprints a stored diarization carries.
///
/// The voiceprints may be empty where the spans are not (a result stored before
/// the shim embedded, or a clip whose slices all failed). That is a normal
/// outcome: the turns get no name guess.
///
/// # Errors
/// `None` when the shim refused or the body is not this shape. Both permanent.
#[must_use]
pub fn voices(stored: &str) -> Option<(Vec<SpeakerTurn>, Vec<SpeakerVoice>)> {
    let reply: Reply<Voices> = serde_json::from_str(stored).ok()?;
    if !reply.ok {
        return None;
    }
    let body = reply.result?;
    Some((body.turns, body.speakers))
}

/// Every word a stored `transcribe-segment` result carries, in order, with the
/// block's detected language.
///
/// Words, not segments: alignment assigns each word to whoever was speaking at
/// its midpoint. A result with no word timings yields `None`, and the caller
/// keeps the transcript it has.
///
/// ⚠ A segment the model looped on, or one without words, contributes nothing.
/// The quality rule applies per segment, as in `turns::plan`, not to the
/// finished turn: alignment often collapses a clip into one turn, and one looped
/// segment would then condemn the clean ones around it.
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
        .filter(|s| {
            !(crate::quality::is_repetition_loop(&s.text) || crate::quality::is_wordless(&s.text))
        })
        .flat_map(|s| s.words)
        .filter(|w| w.end > w.start)
        .collect();
    (!words.is_empty()).then_some((words, outcome.language))
}

/// Did this stored transcription carry ANY word timings, before the quality
/// filter in [`words_of`] had its say?
///
/// The two absences differ. No timings at all is transient: a code change can
/// make the result readable. Timings present but every segment filtered is
/// permanent, so the clip is retired with a ledger row rather than left at the
/// head of the queue.
#[must_use]
pub fn has_word_timings(stored: &str) -> bool {
    let Ok(reply) = serde_json::from_str::<Reply<Transcription>>(stored) else {
        return false;
    };
    reply.ok
        && reply
            .result
            .is_some_and(|t| t.segments.iter().any(|s| !s.words.is_empty()))
}

/// The replace arm's write, lifted out so `write_pass` stays under one screen.
///
/// # Errors
/// If the database refuses.
fn write_replacement(
    meaning: &mut rusqlite::Connection,
    swap: &Swap,
    block: &Block<'_>,
    prints: &[SpeakerVoice],
    enrolled: &[crate::identify::Voiceprint],
) -> rusqlite::Result<usize> {
    let named = Named {
        voices: prints
            .iter()
            .map(|p| (p.speaker.as_str(), p.vector.as_slice()))
            .collect(),
        enrolled,
    };
    apply(meaning, block, swap, &named)
}

/// Name turns that already exist, without touching their text or boundaries.
///
/// Nothing is hidden and nothing is inserted: the pass contributes who was
/// talking and keeps the existing segmentation.
///
/// The voiceprint is optional: without one the cluster is recorded and no name
/// is guessed.
///
/// # Errors
/// If the database refuses.
fn attribute(
    conn: &mut rusqlite::Connection,
    to: &[(i64, String)],
    prints: &[SpeakerVoice],
    enrolled: &[crate::identify::Voiceprint],
) -> rusqlite::Result<usize> {
    let tx = conn.transaction()?;
    let mut named = 0;
    for (id, speaker) in to {
        turn_store::set_cluster(&tx, *id, speaker)?;
        // The voiceprint of this turn's speaker, not the block's: several
        // speakers can be named in one pass.
        if let Some(print) = prints.iter().find(|p| p.speaker == *speaker) {
            let guess = crate::identify::match_one(&print.vector, enrolled);
            crate::identify::record(&tx, *id, &print.vector, guess.as_ref())?;
        }
        named += 1;
    }
    tx.commit()?;
    Ok(named)
}

/// Apply a [`Swap::Replace`] to one block. One transaction: the hides, the
/// inserts, their search-index rows and embeddings land together or not at all.
///
/// ⚠ A crash between hide and insert would leave the block blank, and the
/// provenance marker would stop it being picked again. That is why this is not
/// two calls.
///
/// # Errors
/// If the transaction cannot be taken or any statement fails. Nothing is left
/// half-applied.
pub fn apply(
    conn: &mut rusqlite::Connection,
    block: &Block<'_>,
    swap: &Swap,
    named: &Named<'_>,
) -> rusqlite::Result<usize> {
    let Block {
        audio_segment_id,
        start: block_start,
        language,
        model,
        provenance,
        hidden_reason,
        now,
    } = *block;
    let Swap::Replace { insert, hide } = swap else {
        return Ok(0);
    };
    if insert.is_empty() {
        return Ok(0);
    }
    let trusted = reliable_language(language);
    let tx = conn.transaction()?;
    for id in hide {
        turn_store::hide(&tx, *id, hidden_reason)?;
    }
    let mut written = 0;
    for turn in insert {
        // Word timings are re-based to the turn's start, so a later boundary
        // edit can snap to a real word time.
        let rebased: Vec<Stored> = turn
            .words
            .iter()
            .map(|w| Stored {
                s: w.start - turn.start,
                e: w.end - turn.start,
                w: w.text.clone(),
            })
            .collect();
        // From `rebased`, not the shim's array: the rate rule reads the stored,
        // turn-relative encoding.
        let spans: Vec<(f64, f64)> = rebased.iter().map(|w| (w.s, w.e)).collect();
        // Zero the confidence for an unexpected block language, a foreign
        // script, or implausibly slow speech; the turn is kept. The script check
        // catches the model contradicting its own `nl`/`en` label.
        let confidence = if trusted
            && !crate::quality::is_foreign_script(&turn.text)
            && !crate::quality::is_implausibly_slow(&spans)
        {
            turn.confidence
        } else {
            0.0
        };
        let words = serde_json::to_string(&rebased)
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;
        let (start, end) = (
            Stamp::of(at(block_start, turn.start)),
            Stamp::of(at(block_start, turn.end)),
        );
        let id = turn_store::insert(
            &tx,
            &NewTurn {
                audio_segment_id: Some(audio_segment_id),
                language,
                asr_confidence: Some(confidence),
                asr_model: Some(model),
                speaker_cluster: Some(&turn.speaker),
                provenance: Some(provenance.clone()),
                word_timings: Some(&words),
                created_utc: Some(now),
                ..NewTurn::at(&start, &end, &turn.text)
            },
        )?;
        // ⚠ In the same transaction as the turn: `rematch::run_once` reads
        // `transcript_embeddings`, so a turn without its embedding can never be
        // named later.
        if let Some(vector) = named.voices.get(turn.speaker.as_str()) {
            let guess = crate::identify::match_one(vector, named.enrolled);
            crate::identify::record(&tx, id, vector, guess.as_ref())?;
        }
        written += 1;
    }
    tx.commit()?;
    Ok(written)
}

/// The one clip a write is about. Grouped because these fields always travel
/// together.
#[derive(Debug, Clone, Copy)]
pub struct Block<'a> {
    pub audio_segment_id: i64,
    /// Where the clip begins in absolute time; the shim's offsets are relative
    /// to it.
    pub start: DateTime<Utc>,
    /// The whole-clip language detection, or `None`. Outside
    /// [`HOUSEHOLD_LANGUAGES`] a turn loses its confidence.
    pub language: Option<&'a str>,
    pub model: &'a str,
    /// The pass's reversal key — see [`PROVENANCE`].
    pub provenance: &'a Provenance,
    /// What this pass records on the turns it supersedes.
    pub hidden_reason: &'a HiddenReason,
    pub now: &'a Stamp,
}

/// What a pass needs to put a name to the speakers it writes: the clip's own
/// voiceprints, and the people already enrolled.
///
/// Empty `voices` is ordinary, not an error: those turns land with their
/// `SPEAKER_nn` cluster and no guess.
pub struct Named<'a> {
    /// Speaker label from THIS clip's diarization to the vector built for it.
    pub voices: std::collections::HashMap<&'a str, &'a [f64]>,
    pub enrolled: &'a [crate::identify::Voiceprint],
}

/// The machine turns standing on a block, and the spans a person has corrected
/// inside it: the two things [`decide`] needs from the database.
///
/// ⚠ Turns a person owns are excluded: they are never hidden, and counting them
/// would let the coverage guard treat a person's work as something to replace.
///
/// # Errors
/// If the database refuses.
pub fn standing(
    conn: &rusqlite::Connection,
    audio_segment_id: i64,
) -> rusqlite::Result<Vec<Existing>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT id, text, start_utc, end_utc, word_timings FROM transcript_segments
         WHERE audio_segment_id = ?1 AND superseded_by IS NULL
           AND hidden_reason IS NULL AND NOT {HUMAN_OWNED}",
        HUMAN_OWNED = turn_store::HUMAN_OWNED,
    ))?;
    let rows = stmt.query_map([audio_segment_id], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<String>>(4)?,
        ))
    })?;
    // A turn whose stored instants will not parse is skipped, not defaulted to
    // the epoch, where it would silently overlap nothing.
    let mut out = Vec::new();
    for row in rows {
        let (id, text, start, end, word_timings) = row?;
        if let (Ok(start), Ok(end)) = (
            DateTime::parse_from_rfc3339(&start),
            DateTime::parse_from_rfc3339(&end),
        ) {
            out.push(Existing {
                id,
                text,
                start: start.with_timezone(&Utc),
                end: end.with_timezone(&Utc),
                word_timings,
            });
        }
    }
    Ok(out)
}

// --- the pass ----------------------------------------------------------------

/// What this pass's rows record in `provenance`: the reversal key, naming this
/// pass alone. Not `DiarizedAligned(<model>)`: older rows carry that with the
/// same model name, and a reversal could not tell them apart.
pub const PROVENANCE: Provenance = Provenance::DiarizedAligned(Cow::Borrowed("per-mic runner"));
/// What the turns it supersedes record. Unique for the same reason: un-hiding
/// what this pass hid must not disturb anything else.
pub const HIDDEN_REASON: HiddenReason = HiddenReason::DiarizedBy(Cow::Borrowed("per-mic runner"));

/// What one diarized pass did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pass {
    pub blocks: usize,
    pub turns: usize,
    pub hidden: usize,
    /// Blocks whose existing transcript was kept, by refusal. A pass that keeps
    /// most of what it sees is not doing work.
    pub kept: usize,
    /// Blocks waiting on something transient: no audio segment registered yet,
    /// or no word timings this pass can read. These get no ledger row.
    pub waiting: usize,
    /// Turns named in place, where the pass had speakers but no segmentation
    /// worth trading the existing boundaries for.
    pub named: usize,
}

/// A clip whose transcription yields no usable words: decide which absence it is
/// and retire the permanent one.
///
/// `true`: retired with a ledger row, because timings exist but every segment
/// carrying them was filtered, and a stored result does not change. `false`: no
/// word timings this pass can read, which a code change can fix.
///
/// # Errors
/// If the ledger refuses.
fn retire_if_permanently_unusable(
    ingest: &rusqlite::Connection,
    kind: Kind,
    filename: &str,
    transcription: &str,
    now: &Stamp,
) -> rusqlite::Result<bool> {
    if !has_word_timings(transcription) {
        return Ok(false);
    }
    record(
        ingest,
        kind,
        filename,
        Outcome::AllSegmentsLooped,
        None,
        now,
    )?;
    Ok(true)
}

/// Drain the finished diarize jobs into speaker-aligned turns.
///
/// Every swap goes through [`decide`] first and every terminal decision leaves a
/// ledger row. The two transient outcomes get none: a clip whose audio segment
/// is not registered yet, and one whose transcription carries no word timings
/// this pass can read.
pub fn write_pass(
    meaning: &mut rusqlite::Connection,
    ingest: &rusqlite::Connection,
    now: &Stamp,
    limit: usize,
) -> rusqlite::Result<Pass> {
    let model = crate::turns::SHIM_MODEL;
    // The diarize job and the transcription it aligns against, joined on the
    // shared filename so the two results cannot get out of step.
    let mut stmt = ingest.prepare(
        "SELECT d.filename, d.result, t.result, s.source
         FROM jobs d
         JOIN jobs t ON t.filename = d.filename AND t.kind = ?2
                    AND t.done_utc IS NOT NULL AND t.result IS NOT NULL
         JOIN segments s ON s.filename = d.filename
         WHERE d.kind = ?1 AND d.done_utc IS NOT NULL AND d.result IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM pass_ledger l
                           WHERE l.kind = ?1 AND l.filename = d.filename)
         ORDER BY s.start_utc ASC, d.filename ASC",
    )?;
    let jobs: Vec<(String, String, String, String)> = stmt
        .query_map(
            rusqlite::params![Kind::DiarizeSegment, Kind::TranscribeSegment],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?
        .collect::<Result<_, _>>()?;

    let kind = Kind::DiarizeSegment;
    // Loaded once per pass, not per block, so the cost of naming does not scale
    // with the backlog.
    let enrolled = crate::identify::enrolled(meaning)?;
    let mut pass = Pass::default();
    for (filename, stored_voices, transcription, source) in jobs {
        if pass.blocks >= limit {
            break;
        }
        let Some(block_start) = audiocore::names::parse_segment_start(&filename) else {
            record(ingest, kind, &filename, Outcome::Unnameable, None, now)?;
            continue;
        };
        let Some((speakers, prints)) = voices(&stored_voices) else {
            // The shim refused, or sent an unknown shape. Both permanent.
            record(ingest, kind, &filename, Outcome::Unreadable, None, now)?;
            pass.kept += 1;
            continue;
        };
        let Ok((audio_id, end_raw)) = meaning.query_row(
            "SELECT id, end_utc FROM audio_segments
             WHERE source_id = ?1 AND start_utc = ?2",
            rusqlite::params![source, instant::python_isoformat_utc(block_start)],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        ) else {
            // Transient, unless the session was deleted and the audio is never
            // coming.
            if crate::turns::tombstoned_block(meaning, &source, block_start)? {
                record(ingest, kind, &filename, Outcome::Deleted, None, now)?;
            } else {
                pass.waiting += 1;
            }
            continue;
        };
        // No usable words: retire the permanent case, wait on the transient one.
        let Some((words, language)) = words_of(&transcription) else {
            if retire_if_permanently_unusable(ingest, kind, &filename, &transcription, now)? {
                pass.kept += 1;
            } else {
                pass.waiting += 1;
            }
            continue;
        };
        // An unparseable end collapses the window to the block's start, so the
        // correction lookup finds nothing and the pass refuses rather than
        // writing over a span it could not check.
        let block_end = DateTime::parse_from_rfc3339(&end_raw)
            .map_or_else(|_| at(block_start, 0.0), |t| t.with_timezone(&Utc));
        let existing = standing(meaning, audio_id)?;
        let human = turn_store::protected_between(meaning, block_start, block_end)?;
        let aligned = assign_words_to_speakers(&words, &speakers, crate::align::MIN_TURN_S);
        let swap = decide(block_start, aligned, &existing, &human);
        pass.blocks += 1;
        match &swap {
            Swap::Keep(why) => {
                let (outcome, detail) = why.recorded();
                record(ingest, kind, &filename, outcome, detail.as_ref(), now)?;
                pass.kept += 1;
            }
            Swap::Attribute { to } => {
                pass.named += attribute(meaning, to, &prints, &enrolled)?;
                let speakers: std::collections::BTreeSet<&str> =
                    to.iter().map(|(_, s)| s.as_str()).collect();
                let detail = serde_json::json!({ "turns": to.len(), "speakers": speakers.len() });
                record(
                    ingest,
                    kind,
                    &filename,
                    Outcome::Attributed,
                    Some(&detail),
                    now,
                )?;
            }
            Swap::Replace { hide, .. } => {
                let block = Block {
                    audio_segment_id: audio_id,
                    start: block_start,
                    language: language.as_deref(),
                    model,
                    provenance: &PROVENANCE,
                    hidden_reason: &HIDDEN_REASON,
                    now,
                };
                pass.turns += write_replacement(meaning, &swap, &block, &prints, &enrolled)?;
                pass.hidden += hide.len();
                record(ingest, kind, &filename, Outcome::Aligned, None, now)?;
            }
        }
    }
    Ok(pass)
}
