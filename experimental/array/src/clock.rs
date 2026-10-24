//! One mic's clock against the reference's, from the audio: located coarsely
//! on a loud stretch, then every loud 2 s chunk refined, and a line fitted
//! through them. The line maps a reference sample index to the mic's.

use crate::RATE;
use crate::align::{Fit, Frame, fit_drift, gcc_phat_in};

/// Where delays are measured: a hum or hiss outside it would outvote speech.
pub const SPEECH: (f64, f64) = (300.0, 4_000.0);

/// The mic index for reference index `at`.
pub fn mic_index(fit: &Fit, at: f64) -> f64 {
    at + fit.offset + fit.ppm * 1e-6 * at
}

/// `mic` against `reference`, where the segment names put the mic's first
/// sample `named` seconds after the reference's. `None` when no consistent
/// line is found.
pub fn locate(reference: &[f32], mic: &[f32], named: f64) -> Option<Fit> {
    let r = RATE as usize;
    let blocks = reference.len() / r;
    if blocks < 30 {
        return None;
    }
    let energy = |i: usize| -> f64 {
        reference[i * r..(i + 1) * r]
            .iter()
            .map(|v| f64::from(*v).powi(2))
            .sum()
    };
    // Coarse: the loudest 20 s, searched 4 s either side of the names' guess.
    let start = (0..blocks - 20).max_by(|&a, &b| {
        let s = |x: usize| (x..x + 20).map(energy).sum::<f64>();
        s(a).total_cmp(&s(b))
    })?;
    let a = &reference[start * r..(start + 20) * r];
    let guess = (start * r) as f64 - named * RATE;
    let lo = (guess - 4.0 * RATE).max(0.0) as usize;
    let hi = ((guess + 24.0 * RATE) as usize).min(mic.len());
    if hi <= lo + a.len() {
        return None;
    }
    let found = gcc_phat_in(a, &mic[lo..hi], hi - lo, SPEECH)?;
    // `a` begins at index `lag` of the searched stretch.
    let coarse = lo as f64 + found.lag - (start * r) as f64;
    // Fine: the louder half of the 2 s chunks, each ±10 ms around the coarse offset.
    let chunk = 2 * r;
    let mut loud: Vec<usize> = (0..reference.len() / chunk).collect();
    let chunk_energy = |c: usize| -> f64 {
        reference[c * chunk..(c + 1) * chunk]
            .iter()
            .map(|v| f64::from(*v).powi(2))
            .sum()
    };
    loud.sort_by(|&x, &y| chunk_energy(y).total_cmp(&chunk_energy(x)));
    loud.truncate(loud.len() / 2);
    let frames: Vec<Frame> = loud
        .into_iter()
        .filter_map(|c| {
            let at = c * chunk;
            let j = (at as f64 + coarse).round();
            if j < 0.0 || j as usize + chunk > mic.len() {
                return None;
            }
            let j = j as usize;
            let peak = gcc_phat_in(
                &reference[at..at + chunk],
                &mic[j..j + chunk],
                (0.010 * RATE) as usize,
                SPEECH,
            )?;
            Some(Frame {
                at: at as f64,
                lag: j as f64 + peak.lag - at as f64,
                strength: peak.strength,
            })
        })
        .collect();
    // The talker's position moves the delay a few ms; anything further is an echo.
    let fit = fit_drift(&frames, 0.0, 0.003 * RATE)?;
    // Under a minute, drift is a millisecond, hidden under the talker's moves:
    // fit the offset alone.
    if reference.len() < 60 * r {
        return offset_only(&frames, 0.003 * RATE);
    }
    Some(fit)
}

/// The median lag, refined over the frames within `outlier` of it.
fn offset_only(frames: &[Frame], outlier: f64) -> Option<Fit> {
    let mut lags: Vec<f64> = frames.iter().map(|f| f.lag).collect();
    lags.sort_by(f64::total_cmp);
    let median = *lags.get(lags.len() / 2)?;
    let near: Vec<f64> = lags
        .into_iter()
        .filter(|l| (l - median).abs() <= outlier)
        .collect();
    let offset = near.iter().sum::<f64>() / near.len() as f64;
    let rms = (near.iter().map(|l| (l - offset).powi(2)).sum::<f64>() / near.len() as f64).sqrt();
    Some(Fit {
        offset,
        ppm: 0.0,
        used: near.len(),
        residual_ms: rms / RATE * 1000.0,
    })
}
