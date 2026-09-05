//! Per-band SNR-weighted fusion: for each time-frequency bin, weight each
//! aligned source by how far its energy stands above its own noise floor in
//! that band. The design's magnitude tier (docs/audio-plane.md): immune to
//! phone AGC and noise suppression, which break linear-mixture assumptions
//! but not "who hears this sound best".

use crate::stft::Spectrum;

/// Which source a fused bin takes its phase from.
///
/// The sources are aligned to onset accuracy, not to the sample, so their
/// phases at one bin carry no common reference. Taking each bin's phase from
/// whichever source is loudest therefore splices unrelated phases together,
/// and the donor changes between neighbouring bins and frames: the result is
/// incoherent, and an argmax that flips on a rounding difference makes it
/// unstable under recompilation. `Reference` keeps one channel's phase
/// throughout, leaving the other sources to shape magnitude alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseSource {
    /// Keep the phase of one channel, named by its index in `sources`.
    Reference(usize),
    /// Take each bin's phase from the source standing highest above its floor.
    HighestSnr,
}

/// Per-bin noise floor of one source: a low quantile of magnitude across the
/// window's frames. A quantile rather than a minimum, so one dropout frame of
/// digital zeros doesn't declare the whole band noiseless.
pub fn noise_floors(frames: &[Spectrum], quantile: f64) -> Vec<f32> {
    let Some(first) = frames.first() else {
        return Vec::new();
    };
    let mut floors = Vec::with_capacity(first.len());
    let mut magnitudes = Vec::with_capacity(frames.len());
    for bin in 0..first.len() {
        magnitudes.clear();
        magnitudes.extend(frames.iter().map(|f| f[bin].norm()));
        magnitudes.sort_by(f32::total_cmp);
        let index = ((magnitudes.len() - 1) as f64 * quantile) as usize;
        floors.push(magnitudes[index]);
    }
    floors
}

/// Fuse aligned spectrograms into one. `sources[s]` and `floors[s]` belong to
/// source `s`; all spectrograms must share frame count and bin count, and a
/// `PhaseSource::Reference` index must name one of them (the caller aligned
/// and analysed them on one grid). Weight per bin = the source's SNR (power
/// above its own floor) normalised across sources. A bin where every source
/// sits at its floor fuses to the plain average — silence in, silence out,
/// never a division blow-up.
pub fn fuse(
    sources: &[Vec<Spectrum>],
    floors: &[Vec<f32>],
    phase_from: PhaseSource,
) -> Vec<Spectrum> {
    let Some(first) = sources.first() else {
        return Vec::new();
    };
    let frames = first.len();
    let bins = first.first().map_or(0, Vec::len);
    let eps = 1e-12f32;
    let mut fused = Vec::with_capacity(frames);
    for t in 0..frames {
        let mut frame = Vec::with_capacity(bins);
        for k in 0..bins {
            let mut total = 0.0f32;
            let mut magnitude = 0.0f32;
            let mut best = (0.0f32, 0usize);
            let snrs: Vec<f32> = sources
                .iter()
                .zip(floors)
                .map(|(spec, floor)| {
                    let mag = spec[t][k].norm();
                    let f = floor[k].max(eps);
                    (mag * mag) / (f * f)
                })
                .collect();
            for (s, &snr) in snrs.iter().enumerate() {
                total += snr;
                if snr > best.0 {
                    best = (snr, s);
                }
            }
            if total > eps {
                for (s, spec) in sources.iter().enumerate() {
                    magnitude += (snrs[s] / total) * spec[t][k].norm();
                }
            } else {
                // Every source at its floor: plain average.
                for spec in sources {
                    magnitude += spec[t][k].norm() / sources.len() as f32;
                }
            }
            let donor = match phase_from {
                PhaseSource::Reference(index) => index,
                PhaseSource::HighestSnr => best.1,
            };
            let phase = sources[donor][t][k];
            let unit = if phase.norm() > eps {
                phase / phase.norm()
            } else {
                rustfft::num_complex::Complex::new(1.0, 0.0)
            };
            frame.push(unit * magnitude);
        }
        fused.push(frame);
    }
    fused
}

/// How a block's admitted sources become one signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Combine {
    /// Mix every admitted source, weighted per bin by SNR.
    Weighted,
    /// Carry the single source that hears the block best, and drop the rest.
    BestSource,
}

/// How to rank sources against each other when only one may carry a block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceRank {
    /// The level the source hears speech at. Comparable across microphones
    /// only after per-device gain calibration, but it is not fooled by noise
    /// suppression, and it tracks how well the model hears the speaker.
    SpeechLevel,
    /// Speech over the source's own floor. **Rewards noise gating**: measured
    /// on the 2026-06-23 window the phones sit below -70 dB for 90-99% of the
    /// time because their noise suppression emits near-silence between words,
    /// which lifts this ratio to 36.5 dB against the condenser's honest 25.9 —
    /// while the condenser hears the speech itself 21 dB louder.
    SpeechToFloor,
}

/// Rank one source over a block from its envelope, in dB; higher carries the
/// block. Quantiles name what "speech" and "floor" mean here.
///
/// Returns [`f32::NEG_INFINITY`] for an empty envelope, so an absent source
/// never wins a comparison.
pub fn rank_source(
    envelope: &[f32],
    rank: SourceRank,
    speech_quantile: f64,
    floor_quantile: f64,
) -> f32 {
    if envelope.is_empty() {
        return f32::NEG_INFINITY;
    }
    match rank {
        SourceRank::SpeechLevel => {
            let mut sorted = envelope.to_vec();
            sorted.sort_by(f32::total_cmp);
            let speech = sorted[((sorted.len() - 1) as f64 * speech_quantile) as usize];
            20.0 * speech.max(1e-9).log10()
        }
        SourceRank::SpeechToFloor => speech_to_floor_db(envelope, speech_quantile, floor_quantile),
    }
}

/// A source's speech-to-floor ratio over one block, in dB, from its envelope:
/// a high quantile (what it hears when someone talks) over a low one (its own
/// floor). Comparable across microphones of different sensitivity because both
/// terms scale with gain, so a constant AGC gain cancels.
///
/// Returns [`f32::NEG_INFINITY`] for an empty envelope, so an absent source
/// never wins a comparison.
pub fn speech_to_floor_db(envelope: &[f32], speech_quantile: f64, floor_quantile: f64) -> f32 {
    if envelope.is_empty() {
        return f32::NEG_INFINITY;
    }
    let mut sorted = envelope.to_vec();
    sorted.sort_by(f32::total_cmp);
    let at = |q: f64| sorted[((sorted.len() - 1) as f64 * q) as usize];
    let speech = at(speech_quantile);
    let floor = at(floor_quantile).max(1e-9);
    20.0 * (speech.max(1e-9) / floor).log10()
}
