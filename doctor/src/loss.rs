//! Reconcile recorded coverage against capture-lifecycle events to find LOST
//! speech.
//!
//! A hole in the always-on mic's coverage is benign only when capture was
//! deliberately paused across it. The durable pause/resume events
//! (`capture_events`) say when capture was *meant* to be recording: each resume
//! opens an active span, the next pause closes it. Any part of an active span
//! not covered by recorded audio is capture running but producing nothing —
//! UNEXPLAINED, i.e. silently lost, unrecoverable speech. Judged as coverage of
//! the span (not as gaps between segments) so that a span with no segments at
//! all — the crash-loop shape — is caught too.
//!
//! Deliberately conservative: with no events (pre-epoch history) or before the
//! first resume, we make no claim — the reconciler only judges stretches it can
//! account for, so it never cries loss over an old deliberate pause it has no
//! record of.

use crate::capture::{ALWAYS_ON, minutes};
use crate::check::{Check, Verdict, check, worst};
use crate::source::SourceKind;
use chrono::{DateTime, Duration, Utc};
use std::collections::{BTreeMap, BTreeSet};

/// Durable capture-lifecycle event kinds. A closed taxonomy: a typo'd kind
/// would be written and then silently never match here, which is exactly the
/// check that must not fail quietly.
pub const PAUSE: &str = "pause";
pub const RESUME: &str = "resume";
pub const DEAD_WINDOW: &str = "dead_window";

/// The slice of a stored capture event the reconciler reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub utc: DateTime<Utc>,
    pub kind: String,
    pub source_id: Option<String>,
}

/// A stretch capture was meant to be recording (resume -> the next pause).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

/// An unexplained stretch on one source: recorded speech that is simply gone.
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

/// The stretches capture was meant to be recording: each resume opens a span,
/// the next pause closes it, and a trailing resume stays open to `now`. Events
/// before the first resume contribute nothing — we have no basis to call
/// capture active there.
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

/// Coverage intervals sorted and merged (overlaps collapse, running max end).
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

/// Portions of the active spans not covered by any recorded audio — lost speech.
///
/// Coverage-based, not gap-between-segments-based: a span with NO segments at
/// all (capture "active" but producing nothing — the crash-loop shape that cost
/// ninety minutes in June) leaves no between-segments gap, yet is exactly the
/// total loss this check exists for. `min_loss` absorbs boundary slop (a first
/// segment starting a beat after the resume, a pause recorded a beat after the
/// last segment; the sub-second seams between adjacent segments fall out the
/// same way). `settle` excludes the trailing stretch the indexer cannot have
/// caught up with yet — the newest audio is an in-progress segment plus the
/// worker's min-age guard, so a running capture's last few minutes are always
/// uncovered in the store and must never read as loss.
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

fn loss_summary(gaps: usize, lost: Duration, dead: usize) -> String {
    let mut parts = Vec::new();
    if gaps > 0 {
        parts.push(format!(
            "{gaps} unexplained gap(s) totalling {} min",
            minutes(lost)
        ));
    }
    if dead > 0 {
        parts.push(format!("{dead} dead-window(s)"));
    }
    parts.join(", ")
}

/// How loudly to say that this microphone lost speech.
///
/// The same rule `capture_checks` applies to silence: the always-on mic is
/// wired to this machine and has no excuse, a phone is carried out of the house
/// and has several. An unknown device gets the strict verdict — no excuse has
/// been established for it.
fn loss_verdict(kind: Option<SourceKind>) -> Verdict {
    match kind {
        Some(k) if k != ALWAYS_ON => Verdict::Warn,
        _ => Verdict::Fail,
    }
}

/// Did recorded speech go missing while capture was meant to be running — and
/// on which microphone?
///
/// Reported **per device**, like `capture_checks` and `agent_checks` before it,
/// because a single collapsed count answers the wrong question. It says the
/// house lost speech; it cannot say which microphone to go and fix, and it
/// cannot tell a phone that was carried away from the wired mic that has no
/// excuse. A dead phone then reads as loudly as a dead archive, which is how
/// four dead windows on one pixel9 held the whole check red for three days
/// while every other mic recorded perfectly.
///
/// The roll-up keeps the bare `speech-loss` label so its trend survives the
/// split, and takes the worst verdict across devices. Per-device labels are
/// qualified (`speech-loss:usb`) because fleetwatch's mute key is
/// `(source, collector, label)` — label is unique within a collector, and the
/// bare `source_id` is already taken by the per-mic recording checks.
pub fn loss_checks(
    losses: &[Gap],
    dead_windows: &BTreeMap<String, usize>,
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

    // Every registered device, plus any that lost speech without being one:
    // loss on an unknown source must get its own line rather than vanish into
    // the roll-up.
    let source_ids: BTreeSet<&str> = kinds
        .keys()
        .copied()
        .chain(dead_windows.keys().map(String::as_str))
        .chain(gaps_by_source.keys().copied())
        .collect();

    let mut checks = Vec::new();
    let mut hurt: Vec<String> = Vec::new();
    let mut total_lost = Duration::zero();

    for source_id in source_ids {
        let gaps = gaps_by_source.get(source_id).map_or(&[][..], Vec::as_slice);
        let dead = dead_windows.get(source_id).copied().unwrap_or(0);
        let lost = gaps
            .iter()
            .fold(Duration::zero(), |acc, g| acc + g.length());
        total_lost += lost;
        let (verdict, observed) = if gaps.is_empty() && dead == 0 {
            (Verdict::Pass, format!("no unexplained loss in {hours:.0}h"))
        } else {
            let summary = loss_summary(gaps.len(), lost, dead);
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
