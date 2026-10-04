//! A meeting transcript scored against a hand-made word reference (#1470),
//! with every dropped word placed: in time no line covers, or inside a line.
//!
//! The reference is the AMI Meeting Corpus's word layer (`words/<meeting>.<speaker>.words.xml`).
//! A hypothesis is segments with times from the recording's start: Whisper's
//! own output, or the lines recall shows.

use crate::wer::{Errors, words};
use std::collections::BTreeSet;

/// One spoken reference word.
#[derive(Debug, Clone, PartialEq)]
pub struct RefWord {
    pub speaker: String,
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// One hypothesis segment.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// Hesitations dropped from both sides: annotators and Whisper disagree on
/// whether to write them, and they carry nothing a reader looks for.
const FILLERS: &[&str] = &[
    "um", "uh", "uhm", "erm", "er", "hmm", "mm", "mhm", "mmhmm", "hm", "ah", "oh",
];

/// The words of one speaker's AMI word file. Punctuation and truncated words
/// (`trunc="true"`, a word broken off) are not words anyone can transcribe.
pub fn ami_words(speaker: &str, xml: &str) -> Vec<RefWord> {
    let mut out = Vec::new();
    for chunk in xml.split("<w ").skip(1) {
        let Some((attrs, rest)) = chunk.split_once('>') else {
            continue;
        };
        if attrs.ends_with('/')
            || attrs.contains("punc=\"true\"")
            || attrs.contains("trunc=\"true\"")
        {
            continue;
        }
        let text = unescape(rest.split("</w>").next().unwrap_or_default());
        let (Some(start), Some(end)) = (attr(attrs, "starttime"), attr(attrs, "endtime")) else {
            continue;
        };
        for word in spoken(&text) {
            out.push(RefWord {
                speaker: speaker.to_string(),
                start,
                end,
                text: word,
            });
        }
    }
    out
}

/// XML's escapes: AMI writes every apostrophe as `&#39;`, which unread splits
/// "it's" into three words.
fn unescape(text: &str) -> String {
    text.replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn attr(attrs: &str, name: &str) -> Option<f64> {
    let key = format!("{name}=\"");
    let from = attrs.find(&key)? + key.len();
    attrs[from..].split('"').next()?.parse().ok()
}

/// Normalised words without fillers.
pub fn spoken(text: &str) -> Vec<String> {
    words(text)
        .into_iter()
        .filter(|w| !FILLERS.contains(&w.as_str()))
        .collect()
}

/// What happened to each reference word in the alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fate {
    Right,
    /// By the hypothesis word at this index.
    Substituted(usize),
    Deleted,
}

/// Align `reference` to `hypothesis`, returning the errors and each reference
/// word's fate.
pub fn align(reference: &[String], hypothesis: &[String]) -> (Errors, Vec<Fate>) {
    let (n, m) = (reference.len(), hypothesis.len());
    let mut cost = vec![0u32; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in 0..=n {
        cost[at(i, 0)] = i as u32;
    }
    for j in 0..=m {
        cost[at(0, j)] = j as u32;
    }
    for i in 1..=n {
        for j in 1..=m {
            let sub = cost[at(i - 1, j - 1)] + u32::from(reference[i - 1] != hypothesis[j - 1]);
            let del = cost[at(i - 1, j)] + 1;
            let ins = cost[at(i, j - 1)] + 1;
            cost[at(i, j)] = sub.min(del).min(ins);
        }
    }
    let mut fates = vec![Fate::Deleted; n];
    let mut errors = Errors {
        reference: n,
        ..Errors::default()
    };
    let (mut i, mut j) = (n, m);
    while i > 0 || j > 0 {
        if i > 0
            && j > 0
            && cost[at(i, j)]
                == cost[at(i - 1, j - 1)] + u32::from(reference[i - 1] != hypothesis[j - 1])
        {
            if reference[i - 1] == hypothesis[j - 1] {
                fates[i - 1] = Fate::Right;
            } else {
                fates[i - 1] = Fate::Substituted(j - 1);
                errors.substitutions += 1;
            }
            i -= 1;
            j -= 1;
        } else if i > 0 && cost[at(i, j)] == cost[at(i - 1, j)] + 1 {
            errors.deletions += 1;
            i -= 1;
        } else {
            errors.insertions += 1;
            j -= 1;
        }
    }
    (errors, fates)
}

/// A stretch no hypothesis segment covers that holds reference words.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Hole {
    pub start: f64,
    pub end: f64,
    pub words: usize,
}

/// The score, with the dropped words placed.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Report {
    pub errors: Errors,
    pub wer: Option<f64>,
    /// Deleted words in time no segment covers (within `SLACK` seconds).
    pub deleted_uncovered: usize,
    /// Deleted words inside a segment's span.
    pub deleted_covered: usize,
    /// Deleted words said while another speaker was also talking.
    pub deleted_overlapped: usize,
    /// Deleted words repeating the same speaker's previous word ("I I think"):
    /// a stutter a clean transcript leaves out on purpose.
    pub deleted_repeats: usize,
    /// Deleted backchannels ([`BACKCHANNELS`]): agreement noises, said alone.
    pub deleted_backchannels: usize,
    /// The most often deleted words, with counts.
    pub most_deleted: Vec<(String, usize)>,
    /// The most frequent `said -> written` substitutions, with counts.
    pub most_substituted: Vec<(String, usize)>,
    /// Uncovered stretches holding at least `HOLE_WORDS` reference words.
    pub holes: Vec<Hole>,
}

/// Words that, said alone, only signal listening.
pub const BACKCHANNELS: &[&str] = &["yeah", "yes", "okay", "right", "mm", "uh-huh", "yep", "no"];

/// Seconds a segment's edge may miss a word by and still cover it: Whisper's
/// segment times are approximate.
const SLACK: f64 = 0.5;
const HOLE_WORDS: usize = 5;

pub fn score(reference: &[RefWord], hypothesis: &[Segment]) -> Report {
    let mut reference = reference.to_vec();
    reference.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut segments = hypothesis.to_vec();
    segments.sort_by(|a, b| a.start.total_cmp(&b.start));
    let ref_words: Vec<String> = reference.iter().map(|w| w.text.clone()).collect();
    let hyp_words: Vec<String> = segments.iter().flat_map(|s| spoken(&s.text)).collect();
    let (errors, fates) = align(&ref_words, &hyp_words);

    let covered = |w: &RefWord| {
        segments
            .iter()
            .any(|s| s.start - SLACK <= w.end && w.start <= s.end + SLACK)
    };
    let overlapped = |w: &RefWord| {
        reference
            .iter()
            .any(|o| o.speaker != w.speaker && o.start < w.end && w.start < o.end)
    };
    let (mut deleted_uncovered, mut deleted_covered, mut deleted_overlapped) = (0, 0, 0);
    let (mut deleted_repeats, mut deleted_backchannels) = (0, 0);
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    let mut swaps: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for (k, (w, fate)) in reference.iter().zip(&fates).enumerate() {
        if let Fate::Substituted(h) = fate {
            *swaps
                .entry(format!("{} -> {}", w.text, hyp_words[*h]))
                .or_default() += 1;
        }
        if *fate != Fate::Deleted {
            continue;
        }
        *counts.entry(w.text.as_str()).or_default() += 1;
        let previous = reference[..k].iter().rev().find(|p| p.speaker == w.speaker);
        if previous.is_some_and(|p| p.text == w.text) {
            deleted_repeats += 1;
        } else if BACKCHANNELS.contains(&w.text.as_str()) {
            deleted_backchannels += 1;
        }
        if covered(w) {
            deleted_covered += 1;
        } else {
            deleted_uncovered += 1;
        }
        if overlapped(w) {
            deleted_overlapped += 1;
        }
    }

    let mut holes = Vec::new();
    let mut run: Vec<&RefWord> = Vec::new();
    let flush = |run: &mut Vec<&RefWord>, holes: &mut Vec<Hole>| {
        if run.len() >= HOLE_WORDS {
            holes.push(Hole {
                start: run[0].start,
                end: run.iter().map(|w| w.end).fold(0.0, f64::max),
                words: run.len(),
            });
        }
        run.clear();
    };
    for w in &reference {
        if covered(w) {
            flush(&mut run, &mut holes);
        } else {
            run.push(w);
        }
    }
    flush(&mut run, &mut holes);

    let mut most_deleted: Vec<(String, usize)> = counts
        .into_iter()
        .map(|(w, n)| (w.to_string(), n))
        .collect();
    most_deleted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    most_deleted.truncate(20);
    let mut most_substituted: Vec<(String, usize)> = swaps.into_iter().collect();
    most_substituted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    most_substituted.truncate(20);

    Report {
        wer: errors.rate(),
        deleted_repeats,
        deleted_backchannels,
        most_deleted,
        most_substituted,
        errors,
        deleted_uncovered,
        deleted_covered,
        deleted_overlapped,
        holes,
    }
}

/// Segments from a Whisper result: the shim's reply (`{"ok":..,"result":{..}}`)
/// or the bare result (`{"segments": [..]}`).
pub fn whisper_segments(json: &str) -> Result<Vec<Segment>, String> {
    let value: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let result = value.get("result").unwrap_or(&value);
    let segments = result
        .get("segments")
        .and_then(|s| s.as_array())
        .ok_or("no segments in the result")?;
    segments
        .iter()
        .map(|s| serde_json::from_value(s.clone()).map_err(|e| e.to_string()))
        .collect()
}

/// The distinct speakers in a reference.
pub fn speakers(reference: &[RefWord]) -> BTreeSet<&str> {
    reference.iter().map(|w| w.speaker.as_str()).collect()
}
