//! Replacing a block's machine turns with speaker-aligned ones.
//!
//! The pass hides turns and writes over them, so the whole decision is made in
//! [`decide`] before anything is written: a pass replaces a transcript, names
//! it in place, or keeps it, and never empties one.

use crate::align::AlignedTurn;
use crate::ledger::{Outcome, record};
use crate::quality::{Heard, is_repetition_loop};
use crate::turn_store::{self, HiddenReason, NewTurn, Protected, Provenance};
use audiocore::instant::Stamp;
use audiocore::job::Kind;
use chrono::{DateTime, Duration, Utc};
use std::borrow::Cow;

crate::statements! {
    CURRENT_MACHINE_TURNS: Meaning =
        "SELECT id, text, start_utc, end_utc, word_timings FROM transcript_segments
         WHERE audio_segment_id = ?1 AND superseded_by IS NULL
           AND hidden_reason IS NULL AND NOT ", crate::human_owned!();
    /// The diarize jobs ready to decide, each with its transcription. A clip
    /// asked to be transcribed again waits for its new lines (`retranscribe`).
    FINISHED: Ingest =
        "SELECT d.filename, d.result, t.result, s.source
         FROM jobs d
         JOIN jobs t ON t.filename = d.filename AND t.kind = ?2
                    AND t.done_utc IS NOT NULL AND t.result IS NOT NULL
         JOIN segments s ON s.filename = d.filename
         WHERE d.kind = ?1 AND d.done_utc IS NOT NULL AND d.result IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM pass_ledger l
                           WHERE l.kind = ?1 AND l.filename = d.filename)
           AND NOT EXISTS (SELECT 1 FROM retranscribe_requests r
                           WHERE r.filename = d.filename)
         ORDER BY s.start_utc ASC, d.filename ASC";
    AUDIO_SEGMENT: Meaning =
        "SELECT id, end_utc FROM audio_segments
             WHERE source_id = ?1 AND start_utc = ?2";
}

/// A block detected outside these keeps its turns at zero confidence.
pub const HOUSEHOLD_LANGUAGES: [&str; 2] = ["nl", "en"];

/// Below this fraction of the existing text a pass is declined: a truncated
/// or mis-detected decode would hide the good transcript.
pub const MIN_COVERAGE_RATIO: f64 = 0.5;

/// The coverage bar applies only above this many existing characters.
pub const COVERAGE_REF_MIN_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq)]
pub struct Existing {
    pub id: i64,
    pub text: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    /// Without word timings a turn can only be labelled, not split.
    pub word_timings: Option<String>,
}

/// Why a swap was declined, with its counts for the ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// No words, or no speaker spans.
    NothingAligned,
    /// Every turn was dropped: loops are the model hallucinating, corrected
    /// turns the guard working.
    AllFiltered {
        loops: usize,
        corrected: usize,
    },
    Coverage {
        existing: usize,
        new: usize,
    },
    /// Fewer turns than exist, and no speaker span overlaps one to name.
    Undiscriminating {
        produced: usize,
        existing: usize,
        speakers: usize,
    },
}

impl Refusal {
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
    /// Name the existing turns, each with the speaker covering most of it.
    /// Chosen whenever a pass would write fewer turns than it hides: a pass may
    /// split or label, never merge.
    Attribute {
        /// Turn id, speaker.
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

/// Divide `turn` where the speaker changes, keeping every word. `None` when
/// the pieces would not reconstruct the turn's text, there are no timings, or
/// one speaker has every word.
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
    // `by` is in seconds from the block's start.
    let offset = (turn.start - block_start).as_seconds_f64();
    let base = words.first()?.start;

    let mut runs: Vec<(String, Vec<crate::quality::Word>)> = Vec::new();
    for word in words {
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

    // Letters and digits only: the words carry spacing and punctuation
    // differently.
    let rebuilt = squashed(
        &pieces
            .iter()
            .map(|p| p.text.as_str())
            .collect::<Vec<_>>()
            .join(" "),
    );
    (rebuilt == squashed(&turn.text)).then_some(pieces)
}

fn squashed(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

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

/// Seconds `a` and `b` share, or `None` if they do not meet.
fn overlap_seconds(
    a: (DateTime<Utc>, DateTime<Utc>),
    b: (DateTime<Utc>, DateTime<Utc>),
) -> Option<f64> {
    let start = a.0.max(b.0);
    let end = a.1.min(b.1);
    (end > start).then(|| (end - start).as_seconds_f64())
}

fn at(block_start: DateTime<Utc>, offset_s: f64) -> DateTime<Utc> {
    block_start + Duration::milliseconds((offset_s * 1000.0).round() as i64)
}

/// Decide the swap for one block.
///
/// 1. A repetition loop, or a turn inside a human-corrected span, is dropped.
/// 2. If the block has turns and nothing survives, keep.
/// 3. If fewer turns survive than exist, name the existing ones in place; keep
///    if no span overlaps any.
/// 4. If the surviving text is far smaller than what is there, keep. Loops are
///    left out on both sides of the comparison.
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
    let speakers: std::collections::BTreeSet<&str> =
        keep.iter().map(|t| t.speaker.as_str()).collect();
    if keep.len() < existing.len() {
        // A turn no span touches is left unnamed.
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

#[must_use]
pub fn reliable_language(language: Option<&str>) -> bool {
    language.is_some_and(|l| HOUSEHOLD_LANGUAGES.contains(&l))
}

// --- the write ---------------------------------------------------------------

use crate::align::{SpeakerTurn, Word, assign_words_to_speakers};
use audiocore::instant;
use audiocore::shim::{Stored as StoredReply, asr, voices::Diarization};

pub use audiocore::shim::voices::SpeakerVoice;

/// One word as `word_timings` stores it: `{s, e, w}`. Serialising
/// `align::Word` instead gives JSON no reader parses, which looks the same as
/// no timings.
#[derive(serde::Serialize)]
struct Stored {
    s: f64,
    e: f64,
    w: String,
}

/// The speaker spans of a stored diarization; `None` if refused or unreadable.
#[must_use]
pub fn speaker_turns(stored: &str) -> Option<Vec<SpeakerTurn>> {
    Some(voices(stored)?.0)
}

/// The spans and per-speaker voiceprints of a stored diarization; `None` if
/// refused or unreadable. The voiceprints may be empty (older results, or
/// failed slices): the turns then get no guess.
#[must_use]
pub fn voices(stored: &str) -> Option<(Vec<SpeakerTurn>, Vec<SpeakerVoice>)> {
    let body = StoredReply::<Diarization>::parse(stored).ok()?.answer()?;
    Some((body.turns, body.speakers))
}

/// Every word of a stored transcription, with the detected language; `None`
/// without word timings.
///
/// Looped, wordless and invented segments contribute nothing. The rule applies
/// per segment, not per finished turn: alignment often makes one turn of a
/// clip, and one looped segment would condemn the clean ones.
#[must_use]
pub fn words_of(stored: &str, heard: &Heard) -> Option<(Vec<Word>, Option<String>)> {
    let outcome = StoredReply::<asr::Reply>::parse(stored).ok()?.answer()?;
    let words: Vec<Word> = outcome
        .segments
        .iter()
        .filter(|s| {
            !(crate::quality::is_repetition_loop(&s.text)
                || crate::quality::is_wordless(&s.text)
                || invented(s, heard))
        })
        .flat_map(|s| s.words.iter().flatten().map(Word::from))
        .filter(|w| w.end > w.start)
        .collect();
    (!words.is_empty()).then_some((words, outcome.language))
}

/// Whether `heard` says the model invented this segment. Older results have no
/// segment span; its words' is used.
fn invented(segment: &asr::Segment, heard: &Heard) -> bool {
    let words = segment.words.as_deref().unwrap_or_default();
    let first = words.first().map(|w| w.start);
    let last = words.last().map(|w| w.end);
    match (segment.start.or(first), segment.end.or(last)) {
        (Some(start), Some(end)) => heard.invented(&segment.text, start, end),
        _ => false,
    }
}

/// Whether the stored transcription has word timings before filtering. With
/// none, a code change could still read it; with some but all filtered, the
/// clip is retired.
#[must_use]
pub fn has_word_timings(stored: &str) -> bool {
    StoredReply::<asr::Reply>::parse(stored)
        .ok()
        .and_then(StoredReply::answer)
        .is_some_and(|t| {
            t.segments
                .iter()
                .any(|s| s.words.as_ref().is_some_and(|w| !w.is_empty()))
        })
}

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

/// Name existing turns without touching their text or boundaries. Without a
/// voiceprint only the cluster is recorded.
fn attribute(
    conn: &mut rusqlite::Connection,
    to: &[(i64, String)],
    prints: &[SpeakerVoice],
    enrolled: &[crate::identify::Voiceprint],
) -> rusqlite::Result<usize> {
    let tx = crate::sql::write(conn)?;
    let mut named = 0;
    for (id, speaker) in to {
        turn_store::set_cluster(&tx, *id, speaker)?;
        if let Some(print) = prints.iter().find(|p| p.speaker == *speaker) {
            let guess = crate::identify::match_one(&print.vector, enrolled);
            crate::identify::record(&tx, *id, &print.vector, guess.as_ref())?;
        }
        named += 1;
    }
    tx.commit()?;
    Ok(named)
}

/// Apply a [`Swap::Replace`] in one transaction: a crash between hide and
/// insert would leave the block blank and never picked again.
///
/// # Errors
/// If the database refuses; nothing is left half-applied.
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
    let tx = crate::sql::write(conn)?;
    for id in hide {
        turn_store::hide(&tx, *id, hidden_reason)?;
    }
    let mut written = 0;
    for turn in insert {
        // Turn-relative, as stored.
        let rebased: Vec<Stored> = turn
            .words
            .iter()
            .map(|w| Stored {
                s: w.start - turn.start,
                e: w.end - turn.start,
                w: w.text.clone(),
            })
            .collect();
        let spans: Vec<(f64, f64)> = rebased.iter().map(|w| (w.s, w.e)).collect();
        // Doubted turns are kept at zero confidence.
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
        // In the turn's transaction: a turn without its embedding can never be
        // named later (`rematch::run_once`).
        if let Some(vector) = named.voices.get(turn.speaker.as_str()) {
            let guess = crate::identify::match_one(vector, named.enrolled);
            crate::identify::record(&tx, id, vector, guess.as_ref())?;
        }
        written += 1;
    }
    tx.commit()?;
    Ok(written)
}

/// The clip a write is about.
#[derive(Debug, Clone, Copy)]
pub struct Block<'a> {
    pub audio_segment_id: i64,
    /// The shim's offsets are relative to it.
    pub start: DateTime<Utc>,
    /// The whole-clip detection.
    pub language: Option<&'a str>,
    pub model: &'a str,
    pub provenance: &'a Provenance,
    pub hidden_reason: &'a HiddenReason,
    pub now: &'a Stamp,
}

/// The clip's voiceprints by diarized label, and the people enrolled. Empty
/// `voices` leaves turns with their cluster and no guess.
pub struct Named<'a> {
    pub voices: std::collections::HashMap<&'a str, &'a [f64]>,
    pub enrolled: &'a [crate::identify::Voiceprint],
}

/// The machine turns standing on a block. Turns a person owns are excluded:
/// they are never hidden, and must not count toward the coverage guard.
///
/// # Errors
/// If the database refuses.
pub fn standing(
    conn: &rusqlite::Connection,
    audio_segment_id: i64,
) -> rusqlite::Result<Vec<Existing>> {
    let mut stmt = CURRENT_MACHINE_TURNS.prepare(conn)?;
    let rows = stmt.query_map([audio_segment_id], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<String>>(4)?,
        ))
    })?;
    // Unparseable instants are skipped, not defaulted to the epoch.
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

/// This pass's reversal key, unique to it: older rows carry
/// `DiarizedAligned(<model>)` with the same model name.
pub const PROVENANCE: Provenance = Provenance::DiarizedAligned(Cow::Borrowed("per-mic runner"));
/// Unique for the same reason: undoing this pass must disturb nothing else.
pub const HIDDEN_REASON: HiddenReason = HiddenReason::DiarizedBy(Cow::Borrowed("per-mic runner"));

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pass {
    pub blocks: usize,
    pub turns: usize,
    pub hidden: usize,
    /// Blocks kept by refusal.
    pub kept: usize,
    /// Blocks waiting on audio registration or readable timings; no ledger row.
    pub waiting: usize,
    /// Turns named in place.
    pub named: usize,
}

/// `true` if the clip was retired: timings exist but every segment was
/// filtered. `false` if it has no readable timings, which a code change could
/// fix.
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

/// Drain the finished diarize jobs into speaker-aligned turns. Every terminal
/// decision leaves a ledger row; the two transient ones (see [`Pass::waiting`])
/// do not.
pub fn write_pass(
    meaning: &mut rusqlite::Connection,
    ingest: &rusqlite::Connection,
    now: &Stamp,
    limit: usize,
) -> rusqlite::Result<Pass> {
    let model = crate::turns::SHIM_MODEL;
    let mut stmt = FINISHED.prepare(ingest)?;
    let jobs: Vec<(String, String, String, String)> = stmt
        .query_map(
            rusqlite::params![Kind::DiarizeSegment, Kind::TranscribeSegment],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?
        .collect::<Result<_, _>>()?;

    let kind = Kind::DiarizeSegment;
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
            record(ingest, kind, &filename, Outcome::Unreadable, None, now)?;
            pass.kept += 1;
            continue;
        };
        let Ok((audio_id, end_raw)) = AUDIO_SEGMENT.query_row(
            meaning,
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
        let heard = crate::speech::heard(ingest, &filename)?;
        let Some((words, language)) = words_of(&transcription, &heard) else {
            if retire_if_permanently_unusable(ingest, kind, &filename, &transcription, now)? {
                pass.kept += 1;
            } else {
                pass.waiting += 1;
            }
            continue;
        };
        // An unparseable end collapses the window to the block's start.
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
