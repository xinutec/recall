//! Per-band SNR-weighted fusion: for each time-frequency bin, weight each
//! aligned source by how far its energy stands above its own noise floor in
//! that band, and take phase from whichever source stands highest. The
//! design's magnitude tier (docs/audio-plane.md): immune to phone AGC and
//! noise suppression, which break linear-mixture assumptions but not "who
//! hears this sound best".

use crate::stft::Spectrum;

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
/// source `s`; all spectrograms must share frame count and bin count (the
/// caller aligned and analysed them on one grid). Weight per bin = the
/// source's SNR (power above its own floor) normalised across sources; phase
/// = the highest-SNR source's. A bin where every source sits at its floor
/// fuses to the plain average — silence in, silence out, never a division
/// blow-up.
pub fn fuse(sources: &[Vec<Spectrum>], floors: &[Vec<f32>]) -> Vec<Spectrum> {
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
            let phase = sources[best.1][t][k];
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
