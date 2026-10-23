//! `array measure`: how well the house's mics line up, and how much of each
//! one's signal the reference's explains, band by band, over one stretch.

use array::RATE;
use array::align::{Fit, Frame, fit_drift, gcc_phat_in};
use array::coherence::band_coherence;
use array::timeline::{Run, runs};
use chrono::{DateTime, Utc};
use clap::Parser;
use std::path::PathBuf;

/// Measure alignment and coherence of `--sources` against `--reference`.
#[derive(Parser)]
struct Args {
    /// The archive root, one directory per source.
    #[arg(long)]
    root: PathBuf,
    #[arg(long)]
    from: DateTime<Utc>,
    #[arg(long)]
    to: DateTime<Utc>,
    #[arg(long)]
    reference: String,
    #[arg(long, value_delimiter = ',')]
    sources: Vec<String>,
    /// Print every coarse frame (at, lag, strength) to stderr, to see what the
    /// fit was given.
    #[arg(long)]
    dump: bool,
}

/// Where speech is: delays are measured here only (`gcc_phat_in`).
const SPEECH: (f64, f64) = (300.0, 4_000.0);

/// The bands coherence is reported in, Hz.
const BANDS: [(f64, f64); 6] = [
    (100.0, 300.0),
    (300.0, 1_000.0),
    (1_000.0, 2_000.0),
    (2_000.0, 4_000.0),
    (4_000.0, 8_000.0),
    (8_000.0, 16_000.0),
];

#[derive(serde::Serialize)]
struct Report {
    source: String,
    run: usize,
    frames: usize,
    fit: Option<Fit>,
    /// Mean coherence with the reference per band, over the speech frames.
    coherence: Vec<f64>,
}

fn secs(at: DateTime<Utc>) -> f64 {
    at.timestamp_micros() as f64 / 1e6
}

/// A mic run's frames against the reference: each `frame` samples of the
/// reference every `hop`, searched within `search` samples of `guess(at)`,
/// the mic index the reference index `at` should match.
fn frames(
    reference: &[f32],
    mic: &[f32],
    frame: usize,
    hop: usize,
    search: usize,
    guess: impl Fn(f64) -> f64,
) -> Vec<Frame> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + frame <= reference.len() {
        let start = guess(at as f64).round();
        if start >= 0.0 && (start as usize) + frame <= mic.len() {
            let s = start as usize;
            if let Some(peak) = gcc_phat_in(
                &reference[at..at + frame],
                &mic[s..s + frame],
                search,
                SPEECH,
            ) {
                out.push(Frame {
                    at: at as f64,
                    lag: start + peak.lag - at as f64,
                    strength: peak.strength,
                });
            }
        }
        at += hop;
    }
    out
}

fn measure(reference: &Run, mic: &Run, dump: bool) -> (usize, Option<Fit>, Vec<f64>) {
    let r = RATE as usize;
    // The names put the mic's run this many samples after the reference's.
    let named = (secs(mic.start) - secs(reference.start)) * RATE;
    // Coarse: 4 s frames, searched ±1.5 s around the names' guess.
    let coarse = frames(
        &reference.samples,
        &mic.samples,
        4 * r,
        2 * r,
        3 * r / 2,
        |at| at - named,
    );
    if dump {
        for f in &coarse {
            eprintln!(
                "{:.1}\t{:.1}\t{:.3}",
                f.at / RATE,
                f.lag / RATE * 1000.0,
                f.strength
            );
        }
    }
    let Some(rough) = fit_drift(&coarse, 0.02, 0.005 * RATE) else {
        return (coarse.len(), None, Vec::new());
    };
    // Fine: 1 s frames, each searched ±30 ms around the clock line. Within a
    // frame the delay is the clock's plus the talker's geometry, and the talker
    // moves between frames, so each frame keeps its own.
    let line = move |at: f64| at + rough.offset + rough.ppm * 1e-6 * at;
    let fine = frames(
        &reference.samples,
        &mic.samples,
        r,
        r / 2,
        (0.030 * RATE) as usize,
        line,
    );
    if dump {
        for f in &fine {
            let off = (f.lag - (rough.offset + rough.ppm * 1e-6 * f.at)) / RATE * 1000.0;
            eprintln!("fine\t{:.1}\t{:.2}\t{:.3}", f.at / RATE, off, f.strength);
        }
    }
    // Coherence over the louder half of the frames, each steered by its own
    // delay: speech, not room tone.
    let mut loud: Vec<&Frame> = fine.iter().collect();
    let energy = |f: &Frame| -> f64 {
        let a = f.at as usize;
        reference.samples[a..a + r]
            .iter()
            .map(|v| f64::from(*v).powi(2))
            .sum()
    };
    loud.sort_by(|a, b| energy(b).total_cmp(&energy(a)));
    loud.truncate(loud.len() / 2);
    let (mut xs, mut ys) = (Vec::new(), Vec::new());
    for f in loud {
        let (a, s) = (f.at as usize, (f.at + f.lag).round());
        if s >= 0.0 && (s as usize) + r <= mic.samples.len() {
            xs.extend_from_slice(&reference.samples[a..a + r]);
            ys.extend_from_slice(&mic.samples[s as usize..s as usize + r]);
        }
    }
    (
        fine.len(),
        Some(rough),
        band_coherence(&xs, &ys, RATE as u32, &BANDS),
    )
}

fn main() {
    let args = Args::parse();
    let reference = runs(&args.root, &args.reference, args.from, args.to, 1.5);
    let Some(reference) = reference.into_iter().max_by_key(|r| r.samples.len()) else {
        eprintln!("array: no audio for {} in the stretch", args.reference);
        std::process::exit(1);
    };
    let self_check = measure(&reference, &reference, false);
    let mut reports = vec![Report {
        source: format!("{} (itself)", args.reference),
        run: 0,
        frames: self_check.0,
        fit: self_check.1,
        coherence: self_check.2,
    }];
    for source in &args.sources {
        for (run, mic) in runs(&args.root, source, args.from, args.to, 1.5)
            .iter()
            .enumerate()
        {
            let (frames, fit, coherence) = measure(&reference, mic, args.dump);
            reports.push(Report {
                source: source.clone(),
                run,
                frames,
                fit,
                coherence,
            });
        }
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&reports).expect("a report serialises")
    );
}
