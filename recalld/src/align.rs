//! Assign a whole-block transcription to diarized speakers, by word timing.
//!
//! The high-quality path, and the reason stage E4 runs two shims over one clip:
//! transcribe the whole block once — full context, so the language is detected
//! reliably and the anti-hallucination decoding works — then diarize it
//! separately and assign each word to whoever was speaking at that moment. The
//! ASR context stays intact while the result still splits by speaker, at word
//! granularity rather than a coarse midpoint.
//!
//! Word timestamps (Whisper) and diarization boundaries (pyannote) are both
//! ~100 ms approximate, so a single word at a speaker boundary — or a one-word
//! backchannel ("yeah") — routinely lands in the wrong span and would become its
//! own spurious turn. After the raw per-word assignment we **smooth**: any run
//! shorter than `MIN_TURN_S` is absorbed into a neighbour. Real speaking turns
//! are longer than that; sub-threshold "turns" are alignment artefacts.
//!
//! ⚠ **This is a PORT of `recall.align`, which is still live.** Both run until
//! the Python refine path retires, so a divergence here is a bug, not a variant:
//! `tests/align.rs` is `tests/test_align.py` case for case, including the
//! jitter-flip case that names the ping-pong bug this smoothing exists for.

use serde::Deserialize;
use std::cmp::Ordering;

/// Runs shorter than this are alignment artefacts (a jitter-flipped word, a
/// backchannel), folded into a neighbour rather than kept as their own turn.
pub const MIN_TURN_S: f64 = 0.5;

/// One word with its timing, as the `asr` shim reports it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Word {
    pub start: f64,
    pub end: f64,
    /// Whisper words carry their own leading space; joined verbatim.
    pub text: String,
    pub probability: f64,
}

/// A contiguous span attributed to one relative speaker, as the `voices` shim
/// reports it. Clip-relative, like the words.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SpeakerTurn {
    pub speaker: String,
    pub start: f64,
    pub end: f64,
}

/// A run of consecutive words attributed to one relative speaker.
#[derive(Debug, Clone, PartialEq)]
pub struct AlignedTurn {
    pub speaker: String,
    pub start: f64,
    pub end: f64,
    pub text: String,
    /// Mean word probability across the run.
    pub confidence: f64,
    /// The run's words, kept for audio-exact boundary edits later.
    pub words: Vec<Word>,
}

/// A mutable run of consecutive words by one speaker, during smoothing.
struct Run {
    speaker: String,
    words: Vec<Word>,
}

impl Run {
    fn duration(&self) -> f64 {
        match (self.words.first(), self.words.last()) {
            (Some(first), Some(last)) => last.end - first.start,
            _ => 0.0,
        }
    }
}

fn total(a: f64, b: f64) -> Ordering {
    a.partial_cmp(&b).unwrap_or(Ordering::Equal)
}

/// The relative speaker talking at `t`: the turn containing it, or — if `t`
/// falls in a gap between turns — the nearest one by edge distance.
fn speaker_at(t: f64, turns: &[SpeakerTurn]) -> Option<&str> {
    if let Some(turn) = turns.iter().find(|tr| tr.start <= t && t <= tr.end) {
        return Some(&turn.speaker);
    }
    // `min_by` keeps the FIRST of equal minima, which is what Python's `min`
    // does — the tie-break is observable when a word sits exactly between two
    // turns, so the two implementations must agree on it.
    turns
        .iter()
        .min_by(|a, b| {
            let da = (a.start - t).abs().min((a.end - t).abs());
            let db = (b.start - t).abs().min((b.end - t).abs());
            total(da, db)
        })
        .map(|turn| turn.speaker.as_str())
}

/// Merge adjacent runs that share a speaker into one.
fn coalesce(runs: Vec<Run>) -> Vec<Run> {
    let mut merged: Vec<Run> = Vec::with_capacity(runs.len());
    for run in runs {
        match merged.last_mut() {
            Some(previous) if previous.speaker == run.speaker => {
                previous.words.extend(run.words);
            }
            _ => merged.push(run),
        }
    }
    merged
}

/// Absorb sub-`min_turn_s` runs into a neighbour and re-coalesce, so a word or
/// two flipped by timestamp jitter (or a backchannel) does not become its own
/// turn. The shortest offender is relabelled to its longer neighbour each pass,
/// until every run clears the threshold (or only one remains).
fn smooth(mut runs: Vec<Run>, min_turn_s: f64) -> Vec<Run> {
    while runs.len() > 1 {
        let Some(index) = runs
            .iter()
            .enumerate()
            .filter(|(_, run)| run.duration() < min_turn_s)
            .min_by(|(_, a), (_, b)| total(a.duration(), b.duration()))
            .map(|(i, _)| i)
        else {
            break;
        };
        let left = index
            .checked_sub(1)
            .map(|i| (runs[i].speaker.clone(), runs[i].duration()));
        let right = runs
            .get(index + 1)
            .map(|run| (run.speaker.clone(), run.duration()));
        let absorbed = match (left, right) {
            (Some((ls, ld)), Some((rs, rd))) => {
                if ld >= rd {
                    ls
                } else {
                    rs
                }
            }
            (Some((ls, _)), None) => ls,
            (None, Some((rs, _))) => rs,
            (None, None) => break,
        };
        runs[index].speaker = absorbed;
        runs = coalesce(runs);
    }
    runs
}

/// Group `words` into per-speaker runs by which diarized turn each word's
/// MIDPOINT falls in, then smooth away sub-`min_turn_s` turns.
///
/// The text is the words joined (Whisper words carry their own leading spaces).
/// `min_turn_s` is a parameter so the attribution eval can sweep it; production
/// passes `MIN_TURN_S`.
#[must_use]
pub fn assign_words_to_speakers(
    words: &[Word],
    turns: &[SpeakerTurn],
    min_turn_s: f64,
) -> Vec<AlignedTurn> {
    if words.is_empty() || turns.is_empty() {
        return Vec::new();
    }
    let mut raw: Vec<Run> = Vec::new();
    for word in words {
        let Some(speaker) = speaker_at(f64::midpoint(word.start, word.end), turns) else {
            continue;
        };
        match raw.last_mut() {
            Some(run) if run.speaker == speaker => run.words.push(word.clone()),
            _ => raw.push(Run {
                speaker: speaker.to_owned(),
                words: vec![word.clone()],
            }),
        }
    }
    smooth(raw, min_turn_s)
        .into_iter()
        .filter_map(|run| {
            let text = run
                .words
                .iter()
                .map(|w| w.text.as_str())
                .collect::<String>()
                .trim()
                .to_owned();
            if text.is_empty() {
                return None;
            }
            let first = run.words.first()?;
            let last = run.words.last()?;
            #[expect(
                clippy::cast_precision_loss,
                reason = "a run's word count is far inside f64's exact integer range"
            )]
            let count = run.words.len() as f64;
            Some(AlignedTurn {
                speaker: run.speaker,
                start: first.start,
                end: last.end,
                text,
                confidence: run.words.iter().map(|w| w.probability).sum::<f64>() / count,
                words: run.words,
            })
        })
        .collect()
}
