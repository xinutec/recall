//! Repetition loops per source, using the production rule.
//!
//! A SQL proxy ("one text repeated 3+ times") is a different question and its
//! numbers are not comparable with `is_repetition_loop`'s. Name the rule beside
//! any figure this produces.
//!
//! Input: `filename|source|minute|quiet_run_s|text`, one line per transcribed
//! segment, dumped from the fleet's ingest plane.

use recalld::quality::is_repetition_loop;
use std::collections::BTreeMap;

#[derive(Default)]
struct Tally {
    segments: usize,
    looping: usize,
    clips: std::collections::BTreeSet<String>,
    looping_clips: std::collections::BTreeSet<String>,
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: loop_census <dump>");
    let text = std::fs::read_to_string(path).expect("dump");
    let mut by_kind: BTreeMap<String, Tally> = BTreeMap::new();

    for line in text.lines() {
        let f: Vec<&str> = line.splitn(5, '|').collect();
        if f.len() < 5 {
            continue;
        }
        let (clip, source, quiet, body) = (f[0], f[1], f[3].parse::<f64>().unwrap_or(-1.0), f[4]);
        // Split by how much of the block was really recorded (#1661).
        let kind = if source == "room" {
            match quiet {
                q if q < 0.0 => "room (coverage unknown)",
                q if q < 1.4 => "room, well covered",
                q if q < 10.0 => "room, partly padded",
                _ => "room, heavily padded",
            }
            .to_owned()
        } else {
            format!("mic: {source}")
        };
        let tally = by_kind.entry(kind).or_default();
        tally.segments += 1;
        tally.clips.insert(clip.to_owned());
        if is_repetition_loop(body) {
            tally.looping += 1;
            tally.looping_clips.insert(clip.to_owned());
        }
    }

    println!(
        "{:26} {:>8} {:>8} {:>8}   {:>6} {:>8}",
        "population", "segments", "looping", "seg %", "clips", "clip %"
    );
    for (kind, t) in &by_kind {
        let seg_pct = 100.0 * t.looping as f64 / t.segments as f64;
        let clip_pct = 100.0 * t.looping_clips.len() as f64 / t.clips.len() as f64;
        println!(
            "{kind:26} {:>8} {:>8} {seg_pct:>7.1}%   {:>6} {clip_pct:>7.1}%",
            t.segments,
            t.looping,
            t.clips.len()
        );
    }
    paired(&text);
}

/// Room against the microphones recording the SAME minute. Unpaired rates cannot
/// answer this: the mics cover different minutes, so an unpaired table compares
/// the rooms each happened to be in.
fn paired(text: &str) {
    // minute -> (room segments, room loops, mic segments, mic loops) for
    // well-covered room blocks only, which is what the builder now produces.
    let mut room: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut mics: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for line in text.lines() {
        let f: Vec<&str> = line.splitn(5, '|').collect();
        if f.len() < 5 {
            continue;
        }
        let (source, minute, quiet, body) = (
            f[1],
            f[2].to_owned(),
            f[3].parse::<f64>().unwrap_or(-1.0),
            f[4],
        );
        let loops = usize::from(is_repetition_loop(body));
        if source == "room" {
            if (0.0..1.4).contains(&quiet) {
                let e = room.entry(minute).or_default();
                e.0 += 1;
                e.1 += loops;
            }
        } else {
            let e = mics.entry(minute).or_default();
            e.0 += 1;
            e.1 += loops;
        }
    }
    let (mut minutes, mut rs, mut rl, mut ms, mut ml) = (0usize, 0usize, 0usize, 0usize, 0usize);
    for (minute, (segs, loops)) in &room {
        let Some((msegs, mloops)) = mics.get(minute) else {
            continue;
        };
        minutes += 1;
        rs += segs;
        rl += loops;
        ms += msegs;
        ml += mloops;
    }
    println!(
        "\nPAIRED on {minutes} minutes where a well-covered room block and per-mic transcripts both exist:"
    );
    println!(
        "  room          {rs:>7} segments  {rl:>6} looping  {:>5.1}%",
        100.0 * rl as f64 / rs as f64
    );
    println!(
        "  the mics      {ms:>7} segments  {ml:>6} looping  {:>5.1}%   (every microphone that minute)",
        100.0 * ml as f64 / ms as f64
    );
}
