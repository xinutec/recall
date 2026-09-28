//! Which hidden lines to show again where a speaker pass left nothing (#1844).
//!
//! A line a pass hid is an orphan when no visible line on any mic overlaps it:
//! the pass's own replacement skipped those seconds. Where several passes
//! stacked over one span, only the newest hidden version comes back. The
//! write-time rules then keep out what they would have refused on writing:
//! loops, foreign script, implausibly slow speech, and silence phrases where
//! no speech was heard.
//!
//! Reads three JSON arrays on stdin, one per line: the hidden candidates, the
//! visible spans, and the speech pass's rows. Prints the plan, a sample, and
//! the ids to restore (`IDS <id>,<id>,…`).
//!
//! Usage: `… | cargo run --example orphan_restore_plan`

use audiocore::text::is_repetition_loop;
use chrono::{DateTime, Utc};
use recalld::quality::{Heard, is_foreign_script, is_implausibly_slow, word_spans};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::io::BufRead;

#[derive(Deserialize)]
struct Hidden {
    id: i64,
    clip: i64,
    file: String,
    clip_start: String,
    start: String,
    end: String,
    provenance: Option<String>,
    text: String,
    timings: Option<String>,
}

#[derive(Deserialize)]
struct Span {
    start: String,
    end: String,
}

#[derive(Deserialize)]
struct Speech {
    file: String,
    seconds: f64,
    regions: Option<String>,
}

fn at(raw: &str) -> DateTime<Utc> {
    audiocore::instant::parse_utc(raw).expect("a stored instant")
}

fn line<T: for<'de> Deserialize<'de>>(input: &mut impl BufRead) -> T {
    let mut text = String::new();
    input.read_line(&mut text).expect("stdin");
    serde_json::from_str(&text).expect("a JSON array")
}

/// What a line is kept hidden for, if a write-time rule would have refused it.
fn refused(h: &Hidden, heard: &HashMap<&str, Heard>) -> Option<&'static str> {
    let into = |t: &str| (at(t) - at(&h.clip_start)).num_milliseconds() as f64 / 1000.0;
    let words = h.timings.as_deref().map(word_spans).unwrap_or_default();
    if is_repetition_loop(&h.text) {
        Some("repetition loop")
    } else if is_foreign_script(&h.text) {
        Some("foreign script")
    } else if is_implausibly_slow(&words) {
        Some("implausibly slow")
    } else if heard
        .get(h.file.as_str())
        .is_some_and(|heard| heard.invented(&h.text, into(&h.start), into(&h.end)))
    {
        Some("silence phrase, no speech heard")
    } else {
        None
    }
}

/// Hidden lines no visible line on any mic overlaps, per clip.
fn orphans<'a>(hidden: &'a [Hidden], visible: &[Span]) -> BTreeMap<i64, Vec<&'a Hidden>> {
    let mut spans: Vec<(DateTime<Utc>, DateTime<Utc>)> =
        visible.iter().map(|v| (at(&v.start), at(&v.end))).collect();
    spans.sort();
    let longest = spans.iter().map(|(s, e)| *e - *s).max().unwrap_or_default();
    let shown = |start: DateTime<Utc>, end: DateTime<Utc>| {
        let from = spans.partition_point(|(s, _)| *s < start - longest);
        spans[from..]
            .iter()
            .take_while(|(s, _)| *s < end)
            .any(|(s, e)| *s < end && *e > start)
    };
    let mut by_clip: BTreeMap<i64, Vec<&Hidden>> = BTreeMap::new();
    for h in hidden {
        if !shown(at(&h.start), at(&h.end)) {
            by_clip.entry(h.clip).or_default().push(h);
        }
    }
    by_clip
}

/// One clip's orphans in overlapping clusters: versions of the same speech.
fn clusters<'a>(lines: &mut [&'a Hidden]) -> Vec<Vec<&'a Hidden>> {
    lines.sort_by_key(|h| at(&h.start));
    let mut out: Vec<Vec<&Hidden>> = Vec::new();
    let mut reach: Option<DateTime<Utc>> = None;
    for &h in lines.iter() {
        match reach {
            Some(end) if at(&h.start) < end => {
                out.last_mut().expect("open").push(h);
                reach = Some(end.max(at(&h.end)));
            }
            _ => {
                out.push(vec![h]);
                reach = Some(at(&h.end));
            }
        }
    }
    out
}

/// The plan: what comes back, what older versions stay hidden, and what the
/// rules keep hidden, by rule.
struct Plan<'a> {
    restore: Vec<&'a Hidden>,
    stacked: usize,
    skipped: BTreeMap<&'static str, Vec<&'a Hidden>>,
}

fn plan<'a>(
    by_clip: &mut BTreeMap<i64, Vec<&'a Hidden>>,
    heard: &HashMap<&str, Heard>,
) -> Plan<'a> {
    let mut plan = Plan {
        restore: Vec::new(),
        stacked: 0,
        skipped: BTreeMap::new(),
    };
    for lines in by_clip.values_mut() {
        for cluster in clusters(lines) {
            // The newest version: the provenance holding the highest id.
            let newest = cluster
                .iter()
                .max_by_key(|h| h.id)
                .map(|h| h.provenance.clone());
            for h in cluster {
                if Some(h.provenance.clone()) != newest {
                    plan.stacked += 1;
                } else if let Some(rule) = refused(h, heard) {
                    plan.skipped.entry(rule).or_default().push(h);
                } else {
                    plan.restore.push(h);
                }
            }
        }
    }
    plan
}

fn minutes(lines: &[&Hidden]) -> f64 {
    lines
        .iter()
        .map(|h| (at(&h.end) - at(&h.start)).num_milliseconds() as f64 / 60_000.0)
        .sum()
}

fn report(orphans: usize, plan: &Plan) {
    println!(
        "orphans {orphans}; older stacked versions left hidden {}",
        plan.stacked
    );
    println!(
        "restore {} lines, {:.1} min",
        plan.restore.len(),
        minutes(&plan.restore)
    );
    for (rule, lines) in &plan.skipped {
        println!("kept hidden, {rule}: {}", lines.len());
    }
    let mut days: BTreeMap<&str, usize> = BTreeMap::new();
    for h in &plan.restore {
        *days.entry(&h.start[..10]).or_default() += 1;
    }
    println!("by day: {days:?}");
    println!("\nsample to restore:");
    // Every n-th, so the sample spans the days rather than one conversation.
    let step = (plan.restore.len() / 30).max(1);
    for h in plan.restore.iter().step_by(step).take(30) {
        println!(
            "  {} | {}",
            &h.start[..19],
            h.text.chars().take(90).collect::<String>()
        );
    }
    for (rule, lines) in &plan.skipped {
        println!("\nkept hidden ({rule}), first 5:");
        for h in lines.iter().take(5) {
            println!(
                "  {} | {}",
                &h.start[..19],
                h.text.chars().take(90).collect::<String>()
            );
        }
    }
    let ids: Vec<String> = plan.restore.iter().map(|h| h.id.to_string()).collect();
    println!("\nIDS {}", ids.join(","));
}

fn main() {
    let mut input = std::io::stdin().lock();
    let hidden: Vec<Hidden> = line(&mut input);
    let visible: Vec<Span> = line(&mut input);
    let speech: Vec<Speech> = line(&mut input);
    let heard: HashMap<&str, Heard> = speech
        .iter()
        .map(|s| {
            (
                s.file.as_str(),
                Heard {
                    seconds: Some(s.seconds),
                    regions: s
                        .regions
                        .as_deref()
                        .and_then(recalld::speech::parse_regions),
                },
            )
        })
        .collect();
    let mut by_clip = orphans(&hidden, &visible);
    let count = by_clip.values().map(Vec::len).sum();
    let plan = plan(&mut by_clip, &heard);
    report(count, &plan);
}
