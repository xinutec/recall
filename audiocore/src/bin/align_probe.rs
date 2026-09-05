//! Offline tier-1 alignment probe: measure, per wall-clock block, how far each
//! source's archive timestamps sit from a reference source's — from the audio
//! itself, by envelope correlation. The instrument for validating the
//! alignment ladder (docs/audio-plane.md) on real recorded days, including
//! the pre-epoch-fix era whose clocks genuinely disagree.
//!
//!   align-probe --root /Volumes/Backup/recall --reference usb \
//!               --sources pixel5,pixel9 --start 2026-06-23T20:10:00Z \
//!               --minutes 30

use audiocore::align::best_lag;
use audiocore::envelope::{BUCKET_S, DECODE_RATE, rms_buckets};
use audiocore::names::{parse_segment_start, segment_glob};
use chrono::{DateTime, Duration, Utc};
use std::path::Path;
use std::process::ExitCode;

/// Per-block lag search span. June's measured phone skew was ~3.9 s; ±15 s
/// leaves room for worse buffering without letting a spurious far peak win.
const MAX_LAG_S: f64 = 15.0;
/// Analysis block: one nominal segment length.
const BLOCK_S: usize = 60;
/// A verdict needs at least this much shared audio inside a block.
const MIN_OVERLAP_S: f64 = 30.0;

/// Decode one archived segment to s16le mono at the envelope rate.
fn decode(path: &Path) -> Option<Vec<u8>> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(path)
        .args([
            "-ac",
            "1",
            "-ar",
            &DECODE_RATE.to_string(),
            "-f",
            "s16le",
            "-",
        ])
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

/// The window's PCM for one source, placed on the wall clock by segment names
/// (zero-filled where nothing was recorded), so every source shares one
/// nominal timeline and the measured lag is exactly the clock disagreement.
fn window_pcm(root: &Path, source: &str, start: DateTime<Utc>, seconds: usize) -> Vec<u8> {
    let mut buf = vec![0u8; 2 * DECODE_RATE as usize * seconds];
    for path in segment_glob(&root.join(source), source) {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(seg_start) = parse_segment_start(name) else {
            continue;
        };
        // Generously admit anything that could intersect the window.
        if seg_start < start - Duration::seconds(120)
            || seg_start > start + Duration::seconds(seconds as i64)
        {
            continue;
        }
        let Some(pcm) = decode(&path) else { continue };
        let shift = (seg_start - start).num_milliseconds() as f64 / 1000.0;
        let at = (shift * f64::from(DECODE_RATE)) as i64 * 2;
        for (i, byte) in pcm.iter().enumerate() {
            let pos = at + i as i64;
            if pos >= 0 && (pos as usize) < buf.len() {
                buf[pos as usize] = *byte;
            }
        }
    }
    buf
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: align-probe --root <archive> --reference <source> --sources <a,b,..> \
         --start <RFC3339> --minutes <n>"
    );
    ExitCode::FAILURE
}

#[allow(clippy::too_many_lines)]
fn main() -> ExitCode {
    let mut root = None;
    let mut reference = None;
    let mut sources: Vec<String> = Vec::new();
    let mut start: Option<DateTime<Utc>> = None;
    let mut minutes = 10usize;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let Some(value) = args.next() else {
            return usage();
        };
        match arg.as_str() {
            "--root" => root = Some(std::path::PathBuf::from(value)),
            "--reference" => reference = Some(value),
            "--sources" => sources = value.split(',').map(str::to_owned).collect(),
            "--start" => match DateTime::parse_from_rfc3339(&value) {
                Ok(t) => start = Some(t.with_timezone(&Utc)),
                Err(_) => return usage(),
            },
            "--minutes" => match value.parse() {
                Ok(n) => minutes = n,
                Err(_) => return usage(),
            },
            _ => return usage(),
        }
    }
    let (Some(root), Some(reference), Some(start)) = (root, reference, start) else {
        return usage();
    };
    if sources.is_empty() {
        return usage();
    }

    let seconds = minutes * 60;
    let ref_env = rms_buckets(&window_pcm(&root, &reference, start, seconds));
    let max_lag = (MAX_LAG_S / BUCKET_S) as usize;
    let min_overlap = (MIN_OVERLAP_S / BUCKET_S) as usize;
    let block_buckets = (BLOCK_S as f64 / BUCKET_S) as usize;

    println!(
        "reference: {reference}, start: {start}, window: {minutes} min, blocks of {BLOCK_S} s"
    );
    println!(
        "{:<10} {:>8} {:>10} {:>8}",
        "source", "block", "offset_s", "peak_r"
    );
    for source in &sources {
        let env = rms_buckets(&window_pcm(&root, source, start, seconds));
        for (index, (ref_block, src_block)) in ref_env
            .chunks(block_buckets)
            .zip(env.chunks(block_buckets))
            .enumerate()
        {
            match best_lag(ref_block, src_block, max_lag, BUCKET_S, min_overlap) {
                Some(anchor) => println!(
                    "{:<10} {:>8} {:>10.1} {:>8.2}",
                    source, index, anchor.offset_s, anchor.peak_r
                ),
                None => println!("{source:<10} {index:>8}          -        -"),
            }
        }
    }
    ExitCode::SUCCESS
}
