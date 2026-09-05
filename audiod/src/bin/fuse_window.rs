//! Offline fusion driver — the phase-0 instrument (docs/audio-plane.md): fuse
//! a recorded multi-mic window into one WAV, per-band SNR-weighted, aligned
//! per block from the audio itself. Its output goes to the WER bake-off
//! against the human corrections in the same window.
//!
//!   fuse-window --root /Volumes/Backup/recall --reference usb \
//!               --sources pixel5,pixel9 --start 2026-06-23T20:10:00Z \
//!               --minutes 30 --out /tmp/fused.wav

use audiod::align::best_lag;
use audiod::decode::{to_f32, window_pcm};
use audiod::envelope::rms_buckets_at;
use audiod::fuse::{Combine, PhaseSource, SourceRank, fuse, noise_floors, rank_source};
use audiod::stft::Stft;
use audiod::wav::write_mono16;
use chrono::{DateTime, Utc};
use std::path::PathBuf;
use std::process::ExitCode;

/// The rate fusion runs at: what the ASR working copy uses, so the fused
/// output is exactly model input.
const RATE: u32 = 16_000;
/// Fine-alignment envelope bucket: 10 ms — inside a 32 ms analysis frame.
const FINE_BUCKET_S: f64 = 0.01;
/// Per-block lag search span, as in the tier-1 probe.
const MAX_LAG_S: f64 = 15.0;
const BLOCK_S: usize = 60;
/// Below this correlation a source is not hearing this room's minute and is
/// excluded from the block (the tier gate) rather than fused as noise.
const MIN_PEAK_R: f64 = 0.3;
const MIN_OVERLAP_S: f64 = 20.0;
/// STFT frame: 32 ms at 16 kHz, 50% hop.
const FRAME: usize = 512;
/// Noise floor = this quantile of per-bin magnitude across a block.
const FLOOR_QUANTILE: f64 = 0.1;
/// What a source hears when somebody talks: a high quantile of its envelope.
const SPEECH_QUANTILE: f64 = 0.9;

struct Args {
    root: PathBuf,
    reference: String,
    sources: Vec<String>,
    start: DateTime<Utc>,
    minutes: usize,
    out: PathBuf,
    phase_from: PhaseSource,
    combine: Combine,
    rank: SourceRank,
}

fn parse_args() -> Option<Args> {
    let mut root = None;
    let mut reference = None;
    let mut sources = Vec::new();
    let mut start = None;
    let mut minutes = 10usize;
    let mut out = None;
    let mut phase_from = PhaseSource::Reference(0);
    let mut combine = Combine::Weighted;
    let mut rank = SourceRank::SpeechLevel;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args.next()?;
        match arg.as_str() {
            "--root" => root = Some(PathBuf::from(value)),
            "--reference" => reference = Some(value),
            "--sources" => sources = value.split(',').map(str::to_owned).collect(),
            "--start" => {
                start = Some(
                    DateTime::parse_from_rfc3339(&value)
                        .ok()?
                        .with_timezone(&Utc),
                );
            }
            "--minutes" => minutes = value.parse().ok()?,
            "--out" => out = Some(PathBuf::from(value)),
            "--combine" => {
                combine = match value.as_str() {
                    "weighted" => Combine::Weighted,
                    "best-source" => Combine::BestSource,
                    _ => return None,
                };
            }
            "--rank" => {
                rank = match value.as_str() {
                    "speech-level" => SourceRank::SpeechLevel,
                    "speech-to-floor" => SourceRank::SpeechToFloor,
                    _ => return None,
                };
            }
            "--phase" => {
                phase_from = match value.as_str() {
                    "reference" => PhaseSource::Reference(0),
                    "highest-snr" => PhaseSource::HighestSnr,
                    _ => return None,
                };
            }
            _ => return None,
        }
    }
    Some(Args {
        root: root?,
        reference: reference?,
        sources,
        start: start?,
        minutes,
        out: out?,
        phase_from,
        combine,
        rank,
    })
}

fn main() -> ExitCode {
    let Some(args) = parse_args() else {
        eprintln!(
            "usage: fuse-window --root <archive> --reference <source> --sources <a,b,..> \
             --start <RFC3339> --minutes <n> --out <wav> \
             [--phase reference|highest-snr] \\
             [--combine weighted|best-source] [--rank speech-level|speech-to-floor]"
        );
        return ExitCode::FAILURE;
    };
    let seconds = args.minutes * 60;
    let block_len = RATE as usize * BLOCK_S;
    let max_lag = (MAX_LAG_S / FINE_BUCKET_S) as usize;
    let min_overlap = (MIN_OVERLAP_S / FINE_BUCKET_S) as usize;

    // Every channel on one nominal timeline, reference first.
    let mut names = vec![args.reference.clone()];
    names.extend(args.sources.iter().cloned());
    let channels: Vec<Vec<f32>> = names
        .iter()
        .map(|source| to_f32(&window_pcm(&args.root, source, args.start, seconds, RATE)))
        .collect();

    let stft = Stft::new(FRAME);
    let mut fused_out: Vec<f32> = Vec::with_capacity(RATE as usize * seconds);
    println!("block  source       offset_s  peak_r  in");
    for block in 0..seconds / BLOCK_S {
        let range = block * block_len..(block + 1) * block_len;
        let ref_block = &channels[0][range.clone()];
        let ref_env: Vec<f32> = rms_f32(ref_block);
        // The reference is always in; others join if the audio agrees.
        let mut candidates: Vec<(&str, Vec<f32>)> = vec![(&names[0], ref_block.to_vec())];
        for (name, channel) in names[1..].iter().zip(&channels[1..]) {
            let env = rms_f32(&channel[range.clone()]);
            let anchor = best_lag(&ref_env, &env, max_lag, FINE_BUCKET_S, min_overlap);
            let (offset_s, peak_r, admitted) = match anchor {
                Some(a) if a.peak_r >= MIN_PEAK_R => (a.offset_s, a.peak_r, true),
                Some(a) => (a.offset_s, a.peak_r, false),
                None => (f64::NAN, f64::NAN, false),
            };
            println!(
                "{block:>5}  {name:<10} {offset_s:>9.2} {peak_r:>7.2}  {}",
                if admitted { "yes" } else { "NO" }
            );
            if admitted {
                let shift = (offset_s * f64::from(RATE)) as i64;
                candidates.push((name, shifted(channel, range.clone(), shift)));
            }
        }
        if args.combine == Combine::BestSource {
            let best = candidates
                .iter()
                .enumerate()
                .max_by(|a, b| {
                    rank_source(&rms_f32(&a.1.1), args.rank, SPEECH_QUANTILE, FLOOR_QUANTILE)
                        .total_cmp(&rank_source(
                            &rms_f32(&b.1.1),
                            args.rank,
                            SPEECH_QUANTILE,
                            FLOOR_QUANTILE,
                        ))
                })
                .map_or(0, |(index, _)| index);
            println!(
                "{block:>5}  -> carries {} ({:.1} dB)",
                candidates[best].0,
                rank_source(
                    &rms_f32(&candidates[best].1),
                    args.rank,
                    SPEECH_QUANTILE,
                    FLOOR_QUANTILE
                )
            );
            candidates = vec![std::mem::take(&mut candidates[best])];
        }
        let specs: Vec<_> = candidates
            .iter()
            .map(|(_, samples)| stft.analyse(samples))
            .collect();
        let floors: Vec<Vec<f32>> = specs
            .iter()
            .map(|s| noise_floors(s, FLOOR_QUANTILE))
            .collect();
        fused_out.extend(stft.synthesise(&fuse(&specs, &floors, args.phase_from)));
    }
    if let Err(err) = write_mono16(&args.out, RATE, &fused_out) {
        eprintln!("fuse-window: cannot write {}: {err}", args.out.display());
        return ExitCode::FAILURE;
    }
    println!(
        "wrote {} ({:.1} min)",
        args.out.display(),
        fused_out.len() as f64 / f64::from(RATE) / 60.0
    );
    ExitCode::SUCCESS
}

/// 10 ms RMS envelope of f32 samples (via the shared s16 implementation's
/// maths, kept in one place: convert down, measure identically).
fn rms_f32(samples: &[f32]) -> Vec<f32> {
    let pcm: Vec<u8> = samples
        .iter()
        .flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())
        .collect();
    rms_buckets_at(&pcm, RATE, FINE_BUCKET_S)
}

/// The block's samples with the source shifted by `shift` samples (adding the
/// measured offset moves it onto the reference timeline); data outside the
/// window is zero.
fn shifted(channel: &[f32], range: std::ops::Range<usize>, shift: i64) -> Vec<f32> {
    range
        .map(|i| {
            let j = i as i64 - shift;
            if j >= 0 && (j as usize) < channel.len() {
                channel[j as usize]
            } else {
                0.0
            }
        })
        .collect()
}
