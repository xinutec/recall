//! Score a diarization against who was played (#1711).
//!
//! The diarizer labels speakers per clip, so its labels mean nothing across
//! clips: each clip's labels are matched to the readers by overlap, inside
//! that clip. Only turns with a known reader count (`LibriSpeech`; FLEURS names
//! no speakers).

use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// A stretch of time attributed to one speaker.
#[derive(Debug, Clone)]
pub struct Turn {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub speaker: String,
}

/// One clip's diarization: its turns, labels local to the clip.
#[derive(Debug, Clone)]
pub struct Clip {
    pub source: String,
    pub turns: Vec<Turn>,
}

/// A turn spanning two readers for at least this long each is mixed.
pub const MIXED_S: f64 = 1.0;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Score {
    /// Seconds of played speech by a known reader in the clips' spans.
    pub reference_s: f64,
    /// Of those, seconds no diarized turn covers.
    pub uncovered_s: f64,
    /// Seconds where a label sits on a reader other than the one it is matched to.
    pub confused_s: f64,
    pub turns: usize,
    pub mixed_turns: usize,
    /// Clips whose label count differs from the readers heard in them.
    pub clips: usize,
    pub clips_miscounted: usize,
}

fn overlap(a: &Turn, b: &Turn) -> f64 {
    let s = a.start.max(b.start);
    let e = a.end.min(b.end);
    (e - s).as_seconds_f64().max(0.0)
}

/// Score each source's clips against the reference turns.
pub fn score(reference: &[Turn], clips: &[Clip]) -> BTreeMap<String, Score> {
    let mut out: BTreeMap<String, Score> = BTreeMap::new();
    for clip in clips {
        let Some(first) = clip.turns.iter().map(|t| t.start).min() else {
            continue;
        };
        let last = clip.turns.iter().map(|t| t.end).max().unwrap_or(first);
        let score = out.entry(clip.source.clone()).or_default();
        // The reference inside this clip's diarized span.
        let span = Turn {
            start: first,
            end: last,
            speaker: String::new(),
        };
        let refs: Vec<&Turn> = reference
            .iter()
            .filter(|r| overlap(r, &span) > 0.0)
            .collect();
        // Label -> reader -> seconds.
        let mut by_label: BTreeMap<&str, BTreeMap<&str, f64>> = BTreeMap::new();
        for h in &clip.turns {
            score.turns += 1;
            let mut readers = 0;
            for r in &refs {
                let o = overlap(h, r);
                *by_label
                    .entry(&h.speaker)
                    .or_default()
                    .entry(&r.speaker)
                    .or_default() += o;
                if o >= MIXED_S {
                    readers += 1;
                }
            }
            if readers >= 2 {
                score.mixed_turns += 1;
            }
        }
        for r in &refs {
            let clipped = Turn {
                start: r.start.max(first),
                end: r.end.min(last),
                speaker: String::new(),
            };
            let len = (clipped.end - clipped.start).as_seconds_f64().max(0.0);
            let covered: f64 = clip
                .turns
                .iter()
                .map(|h| overlap(h, &clipped))
                .sum::<f64>()
                .min(len);
            score.reference_s += len;
            score.uncovered_s += len - covered;
        }
        for readers in by_label.values() {
            let total: f64 = readers.values().sum();
            let best = readers.values().copied().fold(0.0, f64::max);
            score.confused_s += total - best;
        }
        let heard: BTreeSet<&str> = refs.iter().map(|r| r.speaker.as_str()).collect();
        let labels = by_label
            .iter()
            .filter(|(_, r)| r.values().sum::<f64>() >= MIXED_S)
            .count();
        score.clips += 1;
        if labels != heard.len() {
            score.clips_miscounted += 1;
        }
    }
    out
}
