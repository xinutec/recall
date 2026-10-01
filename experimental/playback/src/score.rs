//! Score what each microphone's transcript says against what was played.
//!
//! A line belongs to the part whose played span holds its midpoint. The parts
//! are separated by tens of seconds of silence, far wider than the few seconds
//! a phone's clock can sit off the Mac's, so a midpoint does not land in the
//! wrong part. Words in lines that land in no part were said by nobody: the
//! transcriber invented them in the silence.

use crate::plan::Plan;
use crate::wer::{Errors, align, words};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// When a part's first sample sounded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Played {
    pub part: String,
    pub device: String,
    pub start: DateTime<Utc>,
}

/// One transcript line from any arm: a microphone, or a way of combining them.
#[derive(Debug, Clone)]
pub struct Line {
    pub source: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PartScore {
    pub source: String,
    pub part: String,
    pub errors: Errors,
}

#[derive(Debug, Clone, Serialize)]
pub struct Invented {
    pub source: String,
    pub words: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub parts: Vec<PartScore>,
    /// Words in no part's span, per source; a source that invented nothing is listed with 0.
    pub invented: Vec<Invented>,
    /// Seconds of the scored window in which nothing was played.
    pub silent_seconds: f64,
}

/// The room lab's output (`experimental/room`, `room transcribe --out`): one
/// JSON line per minute and arm, segment times relative to the minute. Each
/// arm becomes its own source, `room-<arm>`.
pub fn room_lines(jsonl: &str) -> Result<Vec<Line>, String> {
    let mut out = Vec::new();
    for (n, raw) in jsonl
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
    {
        let row: serde_json::Value =
            serde_json::from_str(raw).map_err(|e| format!("line {}: {e}", n + 1))?;
        let block = row["block"]
            .as_str()
            .and_then(|b| DateTime::parse_from_rfc3339(b).ok())
            .ok_or_else(|| format!("line {}: no block instant", n + 1))?
            .with_timezone(&Utc);
        let arm = row["arm"]
            .as_str()
            .ok_or_else(|| format!("line {}: no arm", n + 1))?;
        for seg in row["result"]["segments"].as_array().into_iter().flatten() {
            let (Some(start), Some(end)) = (seg["start"].as_f64(), seg["end"].as_f64()) else {
                return Err(format!("line {}: a segment without times", n + 1));
            };
            out.push(Line {
                source: format!("room-{arm}"),
                start: block + seconds(start),
                end: block + seconds(end),
                text: seg["text"].as_str().unwrap_or_default().to_string(),
            });
        }
    }
    Ok(out)
}

/// A span of `seconds`; negative or unrepresentable spans are zero.
pub fn seconds(seconds: f64) -> Duration {
    std::time::Duration::try_from_secs_f64(seconds)
        .ok()
        .and_then(|d| Duration::from_std(d).ok())
        .unwrap_or_else(Duration::zero)
}

/// When a played part sounded, start to end; `None` if the plan has no such part.
pub fn span(plan: &Plan, played: &Played) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let part = plan.parts.iter().find(|p| p.name == played.part)?;
    Some((played.start, played.start + seconds(part.seconds)))
}

/// Score `lines` (any number of sources) against `plan` as `played`, over the
/// window `[from, to)`. Every source in `lines` gets a row for every played part.
pub fn score(
    plan: &Plan,
    played: &[Played],
    lines: &[Line],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Report {
    let spans: Vec<(&Played, DateTime<Utc>, DateTime<Utc>)> = played
        .iter()
        .filter_map(|p| span(plan, p).map(|(s, e)| (p, s, e)))
        .collect();
    let mut by_source: BTreeMap<&str, Vec<&Line>> = BTreeMap::new();
    for line in lines {
        by_source.entry(&line.source).or_default().push(line);
    }
    let mut parts = Vec::new();
    let mut invented = Vec::new();
    for (source, mut mine) in by_source {
        mine.sort_by_key(|l| l.start);
        let mut heard: Vec<Vec<String>> = vec![Vec::new(); spans.len()];
        let mut stray = 0;
        for line in mine {
            let mid = line.start + (line.end - line.start) / 2;
            if mid < from || mid >= to {
                continue;
            }
            match spans.iter().position(|(_, s, e)| *s <= mid && mid < *e) {
                Some(i) => heard[i].extend(words(&line.text)),
                None => stray += words(&line.text).len(),
            }
        }
        for ((p, _, _), hypothesis) in spans.iter().zip(&heard) {
            let Some(part) = plan.parts.iter().find(|x| x.name == p.part) else {
                continue;
            };
            let reference: Vec<String> = part.turns.iter().flat_map(|t| words(&t.text)).collect();
            parts.push(PartScore {
                source: source.to_string(),
                part: p.part.clone(),
                errors: align(&reference, hypothesis),
            });
        }
        invented.push(Invented {
            source: source.to_string(),
            words: stray,
        });
    }
    let played_seconds: f64 = spans
        .iter()
        .map(|(_, s, e)| ((*e).min(to) - (*s).max(from)).as_seconds_f64().max(0.0))
        .sum();
    Report {
        parts,
        invented,
        silent_seconds: (to - from).as_seconds_f64() - played_seconds,
    }
}

/// Errors summed over a source's parts.
pub fn total(report: &Report, source: &str) -> Errors {
    let mut sum = Errors::default();
    for p in report.parts.iter().filter(|p| p.source == source) {
        sum.add(p.errors);
    }
    sum
}
