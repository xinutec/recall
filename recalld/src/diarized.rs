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

#[derive(Debug, Clone, PartialEq)]
pub struct Existing {
    pub id: i64,
    pub text: String,
    /// The turn's own span, so attribution can follow the diarization's
    /// evidence instead of assuming it covers the clip.
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
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
    /// Every turn was dropped, counted by the filter that dropped it. The two
    /// mean opposite things: a loop is the pass hallucinating on good audio, a
    /// corrected turn is the guard working. Conflating them made 455 refusals
    /// undiagnosable (#1663).
    AllFiltered {
        loops: usize,
        corrected: usize,
    },
    Coverage {
        existing: usize,
        new: usize,
    },
    /// The pass told nobody apart AND would write fewer turns than already
    /// exist. See the guard in [`decide`] for why that is a refusal.
    Undiscriminating {
        produced: usize,
        existing: usize,
        speakers: usize,
    },
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NothingAligned => write!(f, "nothing-aligned"),
            Self::AllFiltered { loops, corrected } => write!(
                f,
                "all-turns-filtered: {loops} repetition loop(s), {corrected} inside a \
                 human-corrected span"
            ),
            Self::Coverage { existing, new } => write!(
                f,
                "coverage-guard: new {new} chars < {:.0}% of existing {existing}",
                MIN_COVERAGE_RATIO * 100.0
            ),
            Self::Undiscriminating {
                produced,
                existing,
                speakers,
            } => write!(
                f,
                "undiscriminating: {speakers} speaker(s) over {produced} turn(s) against \
                 {existing} existing — nothing to add but a name, and re-segmenting \
                 would spend boundaries to buy it"
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Swap {
    /// Hide `hide` and write `insert`, in ONE transaction — a crash between them
    /// would leave the block blank and, because the marker is what keeps it from
    /// being re-picked, never re-derived.
    Replace {
        insert: Vec<AlignedTurn>,
        hide: Vec<i64>,
    },
    /// Name the turns that are already there. No text is rewritten and no
    /// boundary is lost — the pass contributes the one thing it actually knows.
    ///
    /// ⚠ This is what a single-speaker pass should do. Replacing instead cost
    /// 814 clips their segmentation: 3,686 turns hidden, 815 written back, and
    /// 813 of those clips are now ONE turn (#1663).
    Attribute {
        speaker: String,
        to: Vec<i64>,
    },
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
///    marked skipped — garbage preserved, never retried. It held roughly one in
///    ten of the guard-skipped segments that way.
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
    // ⚠ **A pass that distinguishes NOBODY must not flatten a finer transcript.**
    //
    // Measured 2026-09-19 by releasing 20 refused clips: 7 of the 8 that aligned
    // had diarization return exactly ONE speaker, so there was nothing to split.
    // A 13-turn clip became one 629-character block carrying a single name at
    // 0.177 confidence, and a clip with 8 speaker spans totalling 5.6 s was
    // funnelled whole into one turn because every word takes the only span on
    // offer. That is the coarse, sentence-flattening behaviour this stage exists
    // to REPLACE, arrived at from the other side.
    //
    // Two speakers is the whole point of the stage, so it passes. One speaker
    // over a transcript no finer than the pass passes too: no boundary is lost
    // and the turn gains a name. Only the flattening case is refused.
    let speakers: std::collections::BTreeSet<&str> =
        keep.iter().map(|t| t.speaker.as_str()).collect();
    if speakers.len() < 2 && keep.len() < existing.len() {
        // ⚠ Only the turns the diarization ACTUALLY COVERED. Asserting the one
        // speaker across the whole clip is the same over-claim the flattening
        // made: `pixel5-20260908T162259` had 8 spans totalling 5.6 s against a
        // 617-character transcript, and every word took the only span on offer.
        let to: Vec<i64> = existing
            .iter()
            .filter(|o| {
                keep.iter().any(|t| {
                    overlaps(
                        (o.start, o.end),
                        (at(block_start, t.start), at(block_start, t.end)),
                    )
                })
            })
            .map(|o| o.id)
            .collect();
        if let Some(speaker) = speakers.iter().next().filter(|_| !to.is_empty()) {
            return Swap::Attribute {
                speaker: (*speaker).to_owned(),
                to,
            };
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
use chrono::SecondsFormat;
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
    /// The segment's own text — read ONLY to judge whether the model looped on
    /// it. The turns are built from `words`.
    #[serde(default)]
    text: String,
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
    Some(voices(stored)?.0)
}

/// The spans AND the per-speaker voiceprints a stored diarization carries.
///
/// ⚠ The voiceprints may be EMPTY where the spans are not — an older result
/// stored before the shim embedded, or a clip whose slices all failed. That is a
/// turn with no name guess, which is worse than one with a guess and better than
/// a wrong name, so it is a normal outcome rather than an error.
///
/// # Errors
/// `None` when the shim refused or the body is not this shape — both permanent.
#[must_use]
pub fn voices(stored: &str) -> Option<(Vec<SpeakerTurn>, Vec<SpeakerVoice>)> {
    let reply: Reply<Voices> = serde_json::from_str(stored).ok()?;
    if !reply.ok {
        return None;
    }
    let body = reply.result?;
    Some((body.turns, body.speakers))
}

/// Every word a stored `transcribe-room` result carries, in order, with the
/// block's detected language.
///
/// ⚠ **Words, not segments.** Alignment assigns each WORD to whoever was
/// speaking at its midpoint; a segment-level assignment would put a whole
/// sentence on one speaker and is the coarse behaviour stage E4 exists to
/// replace. A result with no word timings therefore yields nothing, and the
/// caller keeps the transcript it has.
///
/// ⚠ **A segment the model LOOPED on contributes no words, and that is where
/// the quality rule has to be applied — not to the finished turn.** Measured
/// 2026-09-19 across 602 refused clips: 25.5% of ASR segments are loops, but
/// 597 of the 602 also carry clean ones. Because alignment collapses a clip into
/// a single turn 87% of the time, one hallucinated run condemned the whole turn
/// and the pass discarded everything — the whole was a loop while the parts were
/// not, in 94.4% of them. Those clips kept their per-mic text and lost only
/// their SPEAKERS: 4,185 turns across them, 16 with a speaker (#1663).
///
/// The per-mic writer never had this defect because `turns::plan` filters per
/// segment. This is the same rule at the same granularity, so the two passes
/// agree on what the model actually said.
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
/// ⚠ The two absences are opposite kinds. No timings at all is TRANSIENT — an
/// older result whose word key this pass could not read once already, which a
/// code change can make eligible again. Timings present but every segment a
/// loop is PERMANENT: the stored result will not change, so the clip must be
/// retired with a ledger row or it sits at the head of the queue for ever.
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
/// ⚠ **The whole point is that nothing is hidden and nothing is inserted.** A
/// single-speaker pass has exactly one thing to contribute — who was talking —
/// and replacing the transcript to deliver it cost 814 clips their segmentation
/// before this existed (#1663).
///
/// The voiceprint is optional: an older diarization stored before the shim
/// embedded leaves the cluster recorded and the name unguessed, which is worse
/// than a name and better than a wrong one.
///
/// # Errors
/// If the database refuses.
fn attribute(
    conn: &mut rusqlite::Connection,
    ids: &[i64],
    speaker: &str,
    vector: Option<&[f64]>,
    enrolled: &[crate::identify::Voiceprint],
) -> rusqlite::Result<usize> {
    let tx = conn.transaction()?;
    let mut named = 0;
    for &id in ids {
        tx.execute(
            "UPDATE transcript_segments SET speaker_cluster = ?1 WHERE id = ?2",
            rusqlite::params![speaker, id],
        )?;
        if let Some(vector) = vector {
            let guess = crate::identify::match_one(vector, enrolled);
            crate::identify::record(&tx, id, vector, guess.as_ref())?;
        }
        named += 1;
    }
    tx.commit()?;
    Ok(named)
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
        tx.execute(
            "UPDATE transcript_segments SET hidden_reason = ?1
             WHERE id = ?2 AND hidden_reason IS NULL",
            (hidden_reason, id),
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
        // ⚠ From `rebased`, not from the shim's own array: these are recalld's
        // `{s,e,w}`, re-based to this turn, which is the only encoding the rate
        // rule may read.
        let spans: Vec<(f64, f64)> = rebased.iter().map(|w| (w.s, w.e)).collect();
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
                // ⚠ **Script outranks the LABEL.** 681 visible turns carry a
                // foreign language label and only 180 are in a foreign SCRIPT —
                // the rest are Dutch and English the model mislabelled, so the
                // label alone would zero real speech. The other way round is the
                // tell that matters: 273 turns are written in Cyrillic or
                // Japanese while LABELLED nl or en, which is the model
                // contradicting itself (#1410).
                if trusted
                    && !crate::quality::is_foreign_script(&turn.text)
                    && !crate::quality::is_implausibly_slow(&spans)
                {
                    turn.confidence
                } else {
                    0.0
                },
                model,
                turn.speaker,
                provenance,
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
        // ⚠ IN the same transaction as the turn. A turn written without its
        // embedding is one no later re-match can reach — `rematch_speaker_guesses`
        // reads `transcript_embeddings`, so a crash between the two leaves a turn
        // permanently unnameable rather than merely unnamed.
        if let Some(vector) = named.voices.get(turn.speaker.as_str()) {
            let guess = crate::identify::match_one(vector, named.enrolled);
            crate::identify::record(&tx, id, vector, guess.as_ref())?;
        }
        written += 1;
    }
    tx.commit()?;
    Ok(written)
}

/// The one clip a write is about. Grouped because these five always travel
/// together and always come from the same row.
#[derive(Debug, Clone, Copy)]
pub struct Block<'a> {
    pub audio_segment_id: i64,
    /// Where the clip begins in absolute time — the shim's offsets are relative
    /// to the clip it was handed and mean nothing without this.
    pub start: DateTime<Utc>,
    /// The whole-clip language detection, or `None`. Outside the household's
    /// languages a turn keeps its audio and loses its confidence.
    pub language: Option<&'a str>,
    pub model: &'a str,
    /// The stream's reversal key — see [`Stream::provenance`].
    pub provenance: &'a str,
    /// What this pass records on the turns it supersedes.
    pub hidden_reason: &'a str,
    pub now: &'a str,
}

/// What a pass needs to put a name to the speakers it writes: the clip's own
/// voiceprints, and the people already enrolled.
///
/// ⚠ Empty `voices` is ORDINARY, not an error — a diarization stored before the
/// shim embedded carries none. Those turns land with their `SPEAKER_nn` cluster
/// and no guess, which is what a reader should see when nothing is known.
pub struct Named<'a> {
    /// Speaker label from THIS clip's diarization to the vector built for it.
    pub voices: std::collections::HashMap<&'a str, &'a [f64]>,
    pub enrolled: &'a [crate::identify::Voiceprint],
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
        "SELECT id, text, start_utc, end_utc FROM transcript_segments
         WHERE audio_segment_id = ?1 AND superseded_by IS NULL
           AND hidden_reason IS NULL AND asr_model <> 'human'",
    )?;
    let rows = stmt.query_map([audio_segment_id], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    // ⚠ A turn whose stored instants will not parse is SKIPPED, not defaulted:
    // a span at the epoch would overlap nothing and quietly go unnamed, which
    // reads exactly like a turn the diarization did not cover.
    let mut out = Vec::new();
    for row in rows {
        let (id, text, start, end) = row?;
        if let (Ok(start), Ok(end)) = (
            DateTime::parse_from_rfc3339(&start),
            DateTime::parse_from_rfc3339(&end),
        ) {
            out.push(Existing {
                id,
                text,
                start: start.with_timezone(&Utc),
                end: end.with_timezone(&Utc),
            });
        }
    }
    Ok(out)
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

/// Which stream a diarized pass refines, and the four things that differ between
/// them. Everything else in this module is shared.
///
/// ⚠ **A `Stream` is the unit of REVERSAL**, like `turns::Stream`: `provenance`
/// must name exactly the rows one pass wrote and no others, or nobody can take
/// it back. The model name is IN it for that reason — see the reversal block in
/// `main.rs`, which says why an exact match and not a `LIKE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stream<'a> {
    /// The queue kind whose stored speaker spans this pass interprets.
    pub diarize_kind: &'a str,
    /// The kind whose stored result carries the WORDS to align them against.
    pub transcribe_kind: &'a str,
    /// What written rows record in `asr_model`.
    pub model: &'a str,
    /// What they record in `provenance` — THE REVERSAL KEY, and it must name
    /// this pass alone.
    ///
    /// ⚠ Keep the `diarized-aligned` prefix: `reads::tier()`,
    /// `audio::render_blocking` and `assign` all test `starts_with` against it,
    /// so losing it makes these turns read as un-diarized to three readers. And
    /// do NOT reuse `refine.py`'s exact string — it wrote `diarized-aligned
    /// (<model>)` with the same model name these rows carry, so sharing it would
    /// leave a reversal able to take both passes' rows or neither.
    pub provenance: &'a str,
    /// What the turns it supersedes record in `hidden_reason`. Named for the same
    /// reason: un-hiding what THIS pass hid must not disturb what refine hid.
    pub hidden_reason: &'a str,
}

/// One MICROPHONE's clip — `refine.py`'s stream, and the one that replaces it.
///
/// ⚠ `model` is the shim's own default, NOT a decorated name: these rows join the
/// same per-microphone corpus `refine.py` has been writing for months, and a
/// reader filtering on `asr_model` must not see the archive split in two on the
/// day the orchestrator changed. Provenance carries "who wrote it" instead.
pub const PER_MIC: Stream<'static> = Stream {
    diarize_kind: crate::queue::DIARIZE_SEGMENT,
    transcribe_kind: crate::queue::TRANSCRIBE_SEGMENT,
    model: crate::turns::SHIM_MODEL,
    provenance: "diarized-aligned (per-mic runner)",
    hidden_reason: "diarized (per-mic runner)",
};

/// The derived room stream. ⚠ **Gated on #1461, and its writer is OFF.**
///
/// Its clips carry no turns, so a pass over them ADDS rather than replaces — and
/// nothing hides the per-mic turns it duplicates. What it produces is therefore a
/// second transcript of every minute, not a better one; measured on a live
/// archive, nearly every turn it wrote overlapped a per-mic turn.
///
/// The gate is whether the room stream is known to beat the per-mic one at all
/// (#1461). Kept because the code is identical to [`PER_MIC`]'s, so the day that
/// is answered, this is what it needs.
pub const ROOM: Stream<'static> = Stream {
    diarize_kind: crate::queue::DIARIZE_ROOM,
    transcribe_kind: crate::queue::TRANSCRIBE_ROOM,
    model: crate::turns::ROOM_MODEL,
    provenance: "diarized-aligned (room runner)",
    hidden_reason: "diarized (room runner)",
};

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
    /// Turns NAMED in place, where the pass had a speaker but no segmentation
    /// worth trading the existing boundaries for.
    pub named: usize,
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
/// A clip whose transcription yields no usable words: decide WHICH absence it is
/// and retire the permanent one.
///
/// `true` = retired with a ledger row, because the words are there and every
/// segment carrying them looped, and a stored result does not change. `false` =
/// no word timings this pass can read, which a later code change can fix, so it
/// is left to be examined again.
///
/// # Errors
/// If the ledger refuses.
fn retire_if_permanently_unusable(
    ingest: &rusqlite::Connection,
    kind: &str,
    filename: &str,
    transcription: &str,
    now: &str,
) -> rusqlite::Result<bool> {
    if !has_word_timings(transcription) {
        return Ok(false);
    }
    crate::turns::ledger(
        ingest,
        kind,
        filename,
        "all-segments-looped: the transcription carries no usable words",
        now,
    )?;
    Ok(true)
}

pub fn write_pass(
    meaning: &mut rusqlite::Connection,
    ingest: &rusqlite::Connection,
    stream: &Stream,
    now: &str,
    limit: usize,
) -> rusqlite::Result<Pass> {
    let model = stream.model;
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
            rusqlite::params![stream.diarize_kind, stream.transcribe_kind],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?
        .collect::<Result<_, _>>()?;

    let kind = stream.diarize_kind;
    // ⚠ Loaded ONCE per pass, not per block: it is the same few hundred vectors
    // every time, and re-reading them per clip would make the cost of naming
    // scale with the backlog rather than with the people.
    let enrolled = crate::identify::enrolled(meaning)?;
    let mut pass = Pass::default();
    for (filename, stored_voices, transcription, source) in jobs {
        if pass.blocks >= limit {
            break;
        }
        let Some(block_start) = audiocore::names::parse_segment_start(&filename) else {
            crate::turns::ledger(ingest, kind, &filename, "unnameable", now)?;
            continue;
        };
        let Some((speakers, prints)) = voices(&stored_voices) else {
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
            // Transient — unless the session was deleted, in which case the
            // audio is never coming and waiting is for ever (#1653).
            if crate::turns::tombstoned_block(meaning, &source, block_start)? {
                crate::turns::ledger(ingest, stream.diarize_kind, &filename, "deleted", now)?;
            } else {
                pass.waiting += 1;
            }
            continue;
        };
        // ⚠ Transient, so no ledger row: a transcription without word timings
        // today may be re-derived with them.
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
        let human = corrections(meaning, block_start, block_end)?;
        let aligned = assign_words_to_speakers(&words, &speakers, crate::align::MIN_TURN_S);
        let swap = decide(block_start, aligned, &existing, &human);
        pass.blocks += 1;
        match &swap {
            Swap::Keep(why) => {
                crate::turns::ledger(ingest, kind, &filename, &why.to_string(), now)?;
                pass.kept += 1;
            }
            Swap::Attribute { speaker, to } => {
                let vector = prints
                    .iter()
                    .find(|p| p.speaker == *speaker)
                    .map(|p| p.vector.as_slice());
                pass.named += attribute(meaning, to, speaker, vector, &enrolled)?;
                let why = format!("attributed: {} turn(s) named in place", to.len());
                crate::turns::ledger(ingest, kind, &filename, &why, now)?;
            }
            Swap::Replace { hide, .. } => {
                let block = Block {
                    audio_segment_id: audio_id,
                    start: block_start,
                    language: language.as_deref(),
                    model,
                    provenance: stream.provenance,
                    hidden_reason: stream.hidden_reason,
                    now,
                };
                pass.turns += write_replacement(meaning, &swap, &block, &prints, &enrolled)?;
                pass.hidden += hide.len();
                crate::turns::ledger(ingest, kind, &filename, "aligned", now)?;
            }
        }
    }
    Ok(pass)
}
