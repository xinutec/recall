//! Dereverberation by multichannel linear prediction (weighted prediction
//! error, WPE): in each frequency band, each mic's late echo is predicted from
//! every mic's frames at least `delay` frames back, and subtracted. The direct
//! sound and early echo, closer than the delay, cannot be predicted from the
//! past and stay. Iterated: the prediction is weighted by the inverse of the
//! current estimate's power, so quiet frames, mostly echo, count for more.
//! Classical statistics: no model, no training.

use crate::stft::Spectrum;
use nalgebra::DMatrix;
use rayon::prelude::*;
use rustfft::num_complex::Complex;

type C = Complex<f64>;

/// How far the prediction reaches.
#[derive(Debug, Clone, Copy)]
pub struct Wpe {
    /// Frames of past used to predict.
    pub taps: usize,
    /// Frames skipped before them: what is closer is kept.
    pub delay: usize,
    pub iterations: usize,
}

/// Every mic's spectra (mics × frames × bins) with the predicted late echo
/// removed, in the same shape.
pub fn dereverberate(spectra: &[Vec<Spectrum>], p: Wpe) -> Vec<Vec<Spectrum>> {
    let mics = spectra.len();
    let frames = spectra.iter().map(Vec::len).min().unwrap_or(0);
    let bins = spectra[0][0].len();
    let per_bin: Vec<Vec<Vec<C>>> = (0..bins)
        .into_par_iter()
        .map(|k| {
            let y: Vec<Vec<C>> = (0..mics)
                .map(|m| (0..frames).map(|t| spectra[m][t][k]).collect())
                .collect();
            bin(&y, p)
        })
        .collect();
    (0..mics)
        .map(|m| {
            (0..frames)
                .map(|t| (0..bins).map(|k| per_bin[k][m][t]).collect())
                .collect()
        })
        .collect()
}

/// One frequency bin: `y[m][t]` in, the same with the late echo removed out.
fn bin(y: &[Vec<C>], p: Wpe) -> Vec<Vec<C>> {
    let mics = y.len();
    let frames = y[0].len();
    let width = mics * p.taps;
    let first = p.delay + p.taps - 1;
    let mut d: Vec<Vec<C>> = y.to_vec();
    if frames <= first {
        return d;
    }
    // The stacked past for frame t: every mic at t - delay, t - delay - 1, …
    let past = |t: usize, out: &mut Vec<C>| {
        out.clear();
        for tap in 0..p.taps {
            for row in y {
                out.push(row[t - p.delay - tap]);
            }
        }
    };
    let mean_power = y.iter().flatten().map(C::norm_sqr).sum::<f64>() / (mics * frames) as f64;
    let floor = mean_power * 1e-6 + 1e-30;
    let mut stack = Vec::with_capacity(width);
    for _ in 0..p.iterations {
        let mut r = DMatrix::<C>::zeros(width, width);
        let mut q = DMatrix::<C>::zeros(width, mics);
        for t in first..frames {
            let power = (0..mics).map(|m| d[m][t].norm_sqr()).sum::<f64>() / mics as f64;
            let weight = 1.0 / power.max(floor);
            past(t, &mut stack);
            for i in 0..width {
                let a = stack[i] * weight;
                for j in i..width {
                    r[(i, j)] += a * stack[j].conj();
                }
                for m in 0..mics {
                    q[(i, m)] += a * y[m][t].conj();
                }
            }
        }
        for i in 0..width {
            for j in 0..i {
                r[(i, j)] = r[(j, i)].conj();
            }
        }
        let load = (0..width).map(|i| r[(i, i)].re).sum::<f64>() / width as f64 * 1e-6 + 1e-30;
        for i in 0..width {
            r[(i, i)] += C::new(load, 0.0);
        }
        let Some(g) = r.cholesky().map(|c| c.solve(&q)) else {
            return d;
        };
        for t in first..frames {
            past(t, &mut stack);
            for m in 0..mics {
                let predicted: C = (0..width).map(|i| g[(i, m)].conj() * stack[i]).sum();
                d[m][t] = y[m][t] - predicted;
            }
        }
    }
    d
}
