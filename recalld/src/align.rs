//! Assign a block's transcription to diarized speakers: each word goes to
//! whoever was speaking at its midpoint.
//!
//! Word timestamps and diarization boundaries are both ~100 ms approximate, so
//! runs shorter than `MIN_TURN_S` are absorbed into a neighbour.

use serde::Deserialize;
use std::cmp::Ordering;

/// Runs shorter than this are jitter or a backchannel.
pub const MIN_TURN_S: f64 = 0.5;

/// One word with its timing, as the `asr` shim reports it.
///
/// Older mlx-whisper results spell `text` as `word` and have no probability;
/// missing that would align no words, which looks like a silent block.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Word {
    pub start: f64,
    pub end: f64,
    /// Whisper words carry their own leading space; joined verbatim.
    #[serde(alias = "word")]
    pub text: String,
    #[serde(default = "unscored")]
    pub probability: f64,
}

impl From<&audiocore::shim::asr::Word> for Word {
    fn from(word: &audiocore::shim::asr::Word) -> Self {
        Self {
            start: word.start,
            end: word.end,
            text: word.text.clone(),
            probability: word.probability.unwrap_or_else(unscored),
        }
    }
}

/// Probability for an unscored word. Not zero, which would drag the turn's
/// mean down for something not measured.
const fn unscored() -> f64 {
    1.0
}

pub use audiocore::shim::voices::SpeakerTurn;

/// A run of consecutive words attributed to one relative speaker.
#[derive(Debug, Clone, PartialEq)]
pub struct AlignedTurn {
    pub speaker: String,
    pub start: f64,
    pub end: f64,
    pub text: String,
    /// Mean word probability across the run.
    pub confidence: f64,
    /// Kept for audio-exact boundary edits.
    pub words: Vec<Word>,
}

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

/// The turn containing `t`, else the nearest by edge distance.
fn speaker_at(t: f64, turns: &[SpeakerTurn]) -> Option<&str> {
    if let Some(turn) = turns.iter().find(|tr| tr.start <= t && t <= tr.end) {
        return Some(&turn.speaker);
    }
    // Ties go to the first turn; `tests/integration/align_parity.rs` pins it.
    turns
        .iter()
        .min_by(|a, b| {
            let da = (a.start - t).abs().min((a.end - t).abs());
            let db = (b.start - t).abs().min((b.end - t).abs());
            total(da, db)
        })
        .map(|turn| turn.speaker.as_str())
}

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

/// Relabel the shortest run under `min_turn_s` to its longer neighbour, until
/// every run clears it or one remains.
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

/// Group `words` into per-speaker runs and smooth them. `min_turn_s` is a
/// parameter so the attribution eval can sweep it; production passes
/// `MIN_TURN_S`.
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
