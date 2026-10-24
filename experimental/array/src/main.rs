//! `array measure`: how well the house's mics line up and how much of the
//! waveform they share. `array combine`: the multichannel Wiener filter over
//! them, written out beside the reference and each mic, with each one's
//! speech-to-background ratio. `array dereverb`: multichannel dereverberation
//! (WPE) at 16 kHz.

use array::RATE;
use array::align::Fit;
use array::clock::{SPEECH, locate, mic_index};
use array::coherence::band_coherence;
use array::decimate::to_16k;
use array::mwf::{Covariances, filter};
use array::resample::onto;
use array::stft::{FRAME, Spectrum, istft, istft_with, stft, stft_with};
use array::timeline::{Run, runs};
use array::wpe::{Wpe, dereverberate};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Args)]
struct Stretch {
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
}

#[derive(Subcommand)]
enum Command {
    /// Each mic's clock against the reference and its coherence per band.
    Measure {
        #[command(flatten)]
        stretch: Stretch,
    },
    /// The Wiener filter over every mic that aligns, written as WAVs to `--out`.
    Combine {
        #[command(flatten)]
        stretch: Stretch,
        #[arg(long)]
        out: PathBuf,
    },
    /// Multichannel dereverberation (WPE) at 16 kHz, the reference's output
    /// beside the reference alone and single-mic WPE, as WAVs to `--out`.
    Dereverb {
        #[command(flatten)]
        stretch: Stretch,
        #[arg(long)]
        out: PathBuf,
        /// Frames of past each prediction uses (8 ms each).
        #[arg(long, default_value_t = 15)]
        taps: usize,
        /// Frames kept from the present: echo closer than this stays.
        #[arg(long, default_value_t = 3)]
        delay: usize,
    },
}

/// WPE's frames at 16 kHz: 32 ms, every 8 ms.
const WPE_FRAME: usize = 512;

const BANDS: [(f64, f64); 5] = [
    (100.0, 300.0),
    (300.0, 1_000.0),
    (1_000.0, 2_000.0),
    (2_000.0, 4_000.0),
    (4_000.0, 8_000.0),
];

fn secs(at: DateTime<Utc>) -> f64 {
    at.timestamp_micros() as f64 / 1e6
}

fn longest(runs: Vec<Run>) -> Option<Run> {
    runs.into_iter().max_by_key(|r| r.samples.len())
}

/// A mic that aligned: its name, its clock and its samples on the reference's timeline.
struct Aligned {
    name: String,
    fit: Fit,
    samples: Vec<f32>,
}

/// The reference, cut to the stretch, and every source that aligns with it.
fn gather(stretch: &Stretch) -> (Vec<f32>, Vec<Aligned>) {
    let Some(reference) = longest(runs(
        &stretch.root,
        &stretch.reference,
        stretch.from,
        stretch.to,
        1.5,
    )) else {
        eprintln!("array: no audio for {} in the stretch", stretch.reference);
        std::process::exit(1);
    };
    // Only the stretch: a run can hold other sources, whose delays differ.
    let run_start = secs(reference.start);
    let skip = ((secs(stretch.from) - secs(reference.start)).max(0.0) * RATE) as usize;
    let keep = ((secs(stretch.to) - secs(stretch.from)) * RATE) as usize;
    let reference = reference.samples
        [skip.min(reference.samples.len())..(skip + keep).min(reference.samples.len())]
        .to_vec();
    let starts = secs(stretch.from).max(run_start);
    let mut aligned = vec![Aligned {
        name: stretch.reference.clone(),
        fit: Fit {
            offset: 0.0,
            ppm: 0.0,
            used: 0,
            residual_ms: 0.0,
        },
        samples: reference.clone(),
    }];
    for source in &stretch.sources {
        let Some(mic) = longest(runs(&stretch.root, source, stretch.from, stretch.to, 1.5)) else {
            eprintln!("array: {source}: no audio in the stretch");
            continue;
        };
        let named = secs(mic.start) - starts;
        let Some(fit) = locate(&reference, &mic.samples, named) else {
            eprintln!("array: {source}: no consistent alignment, left out");
            continue;
        };
        if fit.residual_ms > 2.0 || fit.used < 4 {
            eprintln!("array: {source}: alignment too loose ({fit:?}), left out");
            continue;
        }
        let samples = onto(&mic.samples, |i| mic_index(&fit, i), reference.len());
        aligned.push(Aligned {
            name: source.clone(),
            fit,
            samples,
        });
    }
    (reference, aligned)
}

/// Each 2 s chunk's coherence with `nfft`-sample windows, averaged over the
/// louder half of the reference's chunks.
fn coherence(reference: &[f32], mic: &[f32]) -> Vec<f64> {
    let chunk = 2 * RATE as usize;
    let mut chunks: Vec<usize> = (0..reference.len().min(mic.len()) / chunk).collect();
    let energy = |c: usize| -> f64 {
        reference[c * chunk..(c + 1) * chunk]
            .iter()
            .map(|v| f64::from(*v).powi(2))
            .sum()
    };
    chunks.sort_by(|&a, &b| energy(b).total_cmp(&energy(a)));
    chunks.truncate(chunks.len() / 2);
    let mut sum = vec![0.0; BANDS.len()];
    for &c in &chunks {
        let r = c * chunk..(c + 1) * chunk;
        for (s, v) in sum.iter_mut().zip(band_coherence(
            &reference[r.clone()],
            &mic[r],
            RATE as u32,
            FRAME,
            &BANDS,
        )) {
            *s += v;
        }
    }
    sum.iter().map(|s| s / chunks.len().max(1) as f64).collect()
}

/// Speech-band energy of each frame of `spectra`, sampled at `rate`.
fn frame_energy(spectra: &[Spectrum], rate: f64) -> Vec<f64> {
    let frame = 2 * (spectra[0].len() - 1);
    let bin = |hz: f64| (hz / rate * frame as f64) as usize;
    spectra
        .iter()
        .map(|s| {
            s[bin(SPEECH.0)..bin(SPEECH.1)]
                .iter()
                .map(rustfft::num_complex::Complex::norm_sqr)
                .sum()
        })
        .collect()
}

/// Speech (`Some(true)`): the reference's louder 40% of frames; background
/// (`Some(false)`): its quietest 30%; the rest is left out of both.
fn classify(reference: &[Spectrum], rate: f64) -> impl Fn(usize) -> Option<bool> {
    let energy = frame_energy(reference, rate);
    let mut sorted = energy.clone();
    sorted.sort_by(f64::total_cmp);
    let (quiet, loud) = (sorted[sorted.len() * 3 / 10], sorted[sorted.len() * 6 / 10]);
    move |f| {
        let e = energy[f];
        if e >= loud {
            Some(true)
        } else if e <= quiet {
            Some(false)
        } else {
            None
        }
    }
}

/// Mean speech-band energy in speech frames over that in background frames, dB.
fn speech_to_background(
    spectra: &[Spectrum],
    rate: f64,
    kind: &impl Fn(usize) -> Option<bool>,
) -> f64 {
    let energy = frame_energy(spectra, rate);
    let mean = |want: bool| {
        let v: Vec<f64> = energy
            .iter()
            .enumerate()
            .filter(|(f, _)| kind(*f) == Some(want))
            .map(|(_, e)| *e)
            .collect();
        v.iter().sum::<f64>() / v.len().max(1) as f64
    };
    10.0 * (mean(true) / mean(false)).log10()
}

fn write_wav(path: &Path, samples: &[f32], rate: u32) -> std::io::Result<()> {
    let data: Vec<u8> = samples
        .iter()
        .flat_map(|v| ((v.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())
        .collect();
    let mut out = Vec::with_capacity(44 + data.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    std::fs::write(path, out)
}

fn main() {
    match Args::parse().command {
        Command::Measure { stretch } => measure(&stretch),
        Command::Combine { stretch, out } => combine(&stretch, &out),
        Command::Dereverb {
            stretch,
            out,
            taps,
            delay,
        } => dereverb(&stretch, &out, taps, delay),
    }
}

fn measure(stretch: &Stretch) {
    let (reference, aligned) = gather(stretch);
    println!(
        "{:14} {:>10} {:>8} {:>5} {:>7} | coherence, 171 ms: 100-300 300-1k 1-2k 2-4k 4-8k",
        "mic", "offset s", "ppm", "used", "res ms"
    );
    for mic in &aligned {
        let c = coherence(&reference, &mic.samples);
        println!(
            "{:14} {:>10.4} {:>8.2} {:>5} {:>7.3} |                    {}",
            mic.name,
            mic.fit.offset / RATE,
            mic.fit.ppm,
            mic.fit.used,
            mic.fit.residual_ms,
            c.iter()
                .map(|v| format!("{v:.2}"))
                .collect::<Vec<_>>()
                .join("    ")
        );
    }
}

fn combine(stretch: &Stretch, out: &Path) {
    let (reference, aligned) = gather(stretch);
    let spectra: Vec<Vec<Spectrum>> = aligned.iter().map(|m| stft(&m.samples)).collect();
    let kind = classify(&spectra[0], RATE);
    let cov = Covariances::estimate(&spectra, &kind);
    let combined = filter(&spectra, &cov, 0);
    std::fs::create_dir_all(out).expect("the output directory");
    println!(
        "{} mics; {} speech frames, {} background frames",
        aligned.len(),
        cov.speech_frames,
        cov.noise_frames
    );
    for (mic, s) in aligned.iter().zip(&spectra) {
        println!(
            "{:14} speech-to-background {:5.1} dB",
            mic.name,
            speech_to_background(s, RATE, &kind)
        );
        write_wav(
            &out.join(format!("{}.wav", mic.name)),
            &mic.samples,
            RATE as u32,
        )
        .expect("a wav");
    }
    println!(
        "{:14} speech-to-background {:5.1} dB",
        "combined",
        speech_to_background(&combined, RATE, &kind)
    );
    write_wav(
        &out.join("combined.wav"),
        &istft(&combined, reference.len()),
        RATE as u32,
    )
    .expect("a wav");
}

fn dereverb(stretch: &Stretch, out: &Path, taps: usize, delay: usize) {
    let (_, aligned) = gather(stretch);
    let low: Vec<Vec<f32>> = aligned.iter().map(|m| to_16k(&m.samples)).collect();
    let spectra: Vec<Vec<Spectrum>> = low.iter().map(|x| stft_with(x, WPE_FRAME)).collect();
    let wpe = Wpe {
        taps,
        delay,
        iterations: 3,
    };
    let kind = classify(&spectra[0], 16_000.0);
    std::fs::create_dir_all(out).expect("the output directory");
    let report = |name: &str, s: &[Spectrum], samples: &[f32]| {
        println!(
            "{name:22} speech-to-quiet {:5.1} dB",
            speech_to_background(s, 16_000.0, &kind)
        );
        write_wav(&out.join(format!("{name}.wav")), samples, 16_000).expect("a wav");
    };
    let len = low[0].len();
    println!(
        "{} mics aligned; WPE {taps} taps after {delay} frames",
        aligned.len()
    );
    report(&format!("{}-raw", aligned[0].name), &spectra[0], &low[0]);
    let alone = dereverberate(&spectra[..1], wpe);
    report(
        &format!("{}-wpe-alone", aligned[0].name),
        &alone[0],
        &istft_with(&alone[0], len, WPE_FRAME),
    );
    let all = dereverberate(&spectra, wpe);
    report(
        &format!("{}-wpe-all", aligned[0].name),
        &all[0],
        &istft_with(&all[0], len, WPE_FRAME),
    );
}
