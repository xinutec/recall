//! Lost speech: stretches when capture was meant to record, by the capture
//! log's pause and resume events, that no recorded audio covers. Before the
//! first resume it claims nothing.

use crate::capture::{ALWAYS_ON, minutes};
use crate::check::{Check, Verdict, check, worst};
use crate::source::SourceKind;
use chrono::{DateTime, Duration, Utc};
use std::collections::{BTreeMap, BTreeSet};

pub use audiocore::capture_log::{PAUSE, RESUME};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub utc: DateTime<Utc>,
    pub kind: String,
    pub source_id: Option<String>,
}

/// A stretch capture was meant to be recording: a resume to the next pause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

/// An uncovered stretch on one source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gap {
    pub source_id: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl Gap {
    fn length(&self) -> Duration {
        self.end - self.start
    }
}

/// Each resume opens a span and the next pause closes it; a trailing resume
/// runs to `now`. Pauses before the first resume are ignored.
pub fn active_spans(events: &[Event], now: DateTime<Utc>) -> Vec<Span> {
    let mut ordered: Vec<&Event> = events.iter().collect();
    ordered.sort_by_key(|e| e.utc);
    let mut spans = Vec::new();
    let mut open_start: Option<DateTime<Utc>> = None;
    for event in ordered {
        if event.kind == RESUME {
            if open_start.is_none() {
                open_start = Some(event.utc);
            }
        } else if event.kind == PAUSE
            && let Some(start) = open_start.take()
        {
            spans.push(Span {
                start,
                end: event.utc,
            });
        }
    }
    if let Some(start) = open_start {
        spans.push(Span { start, end: now });
    }
    spans
}

/// Intervals sorted, overlapping ones merged.
fn merged(intervals: &[(DateTime<Utc>, DateTime<Utc>)]) -> Vec<(DateTime<Utc>, DateTime<Utc>)> {
    let mut sorted = intervals.to_vec();
    sorted.sort();
    let mut out: Vec<(DateTime<Utc>, DateTime<Utc>)> = Vec::new();
    for (start, end) in sorted {
        match out.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => out.push((start, end)),
        }
    }
    out
}

/// The parts of the active spans no recorded audio covers. Measured against
/// coverage, not as gaps between segments, so a span with no segments at all
/// (a crash loop) is lost in full.
///
/// Shorter than `min_loss` is boundary slop. The last `settle` before `now` is
/// not judged: its segment may still be written.
pub fn uncovered_loss(
    intervals: &[(DateTime<Utc>, DateTime<Utc>)],
    events: &[Event],
    source_id: &str,
    now: DateTime<Utc>,
    min_loss: Duration,
    settle: Duration,
) -> Vec<Gap> {
    let horizon = now - settle;
    let coverage = merged(intervals);
    let mut losses = Vec::new();
    for span in active_spans(events, now) {
        let end = span.end.min(horizon);
        let mut cursor = span.start;
        for &(start, stop) in &coverage {
            if stop <= cursor {
                continue;
            }
            if start >= end {
                break;
            }
            if start - cursor >= min_loss {
                losses.push(Gap {
                    source_id: source_id.to_owned(),
                    start: cursor,
                    end: start,
                });
            }
            cursor = cursor.max(stop);
        }
        if end - cursor >= min_loss {
            losses.push(Gap {
                source_id: source_id.to_owned(),
                start: cursor,
                end,
            });
        }
    }
    losses.sort_by_key(|g| g.start);
    losses
}

const LOSS_EXPECTED: &str = "every gap explained by a deliberate pause";

fn loss_summary(gaps: usize, lost: Duration) -> String {
    format!("{gaps} unexplained gap(s) totalling {} min", minutes(lost))
}

/// As for silence: the always-on mic fails, a phone warns, an unknown source
/// fails.
fn loss_verdict(kind: Option<SourceKind>) -> Verdict {
    match kind {
        Some(k) if k != ALWAYS_ON => Verdict::Warn,
        _ => Verdict::Fail,
    }
}

/// One check per device (`speech-loss:<source>`; the bare source id is the
/// recording check's label) and a `speech-loss` roll-up with the worst
/// verdict.
pub fn loss_checks(
    losses: &[Gap],
    sources: &[(String, SourceKind)],
    window: Duration,
) -> Vec<Check> {
    let hours = (window.num_seconds() as f64 / 3600.0).round();
    let kinds: BTreeMap<&str, SourceKind> =
        sources.iter().map(|(id, k)| (id.as_str(), *k)).collect();

    let mut gaps_by_source: BTreeMap<&str, Vec<&Gap>> = BTreeMap::new();
    for gap in losses {
        gaps_by_source
            .entry(gap.source_id.as_str())
            .or_default()
            .push(gap);
    }

    // Registered devices, plus any unregistered source that lost speech.
    let source_ids: BTreeSet<&str> = kinds
        .keys()
        .copied()
        .chain(gaps_by_source.keys().copied())
        .collect();

    let mut checks = Vec::new();
    let mut hurt: Vec<String> = Vec::new();
    let mut total_lost = Duration::zero();

    for source_id in source_ids {
        let gaps = gaps_by_source.get(source_id).map_or(&[][..], Vec::as_slice);
        let lost = gaps
            .iter()
            .fold(Duration::zero(), |acc, g| acc + g.length());
        total_lost += lost;
        let (verdict, observed) = if gaps.is_empty() {
            (Verdict::Pass, format!("no unexplained loss in {hours:.0}h"))
        } else {
            let summary = loss_summary(gaps.len(), lost);
            hurt.push(format!("{source_id}: {summary}"));
            (
                loss_verdict(kinds.get(source_id).copied()),
                format!("{summary} in {hours:.0}h"),
            )
        };
        checks.push(
            check(
                "capture",
                format!("speech-loss:{source_id}"),
                verdict,
                observed,
                LOSS_EXPECTED,
            )
            .trend(minutes(lost), "min")
            .build(),
        );
    }

    let roll_up = worst(checks.iter().map(|c| c.verdict));
    checks.push(
        check(
            "capture",
            "speech-loss",
            roll_up,
            if hurt.is_empty() {
                format!("no unexplained loss in {hours:.0}h")
            } else {
                hurt.join("; ")
            },
            LOSS_EXPECTED,
        )
        .trend(minutes(total_lost), "min")
        .build(),
    );
    checks
}
