//! A multichannel Wiener filter: per frequency, the speech as the reference mic
//! hears it, estimated from every mic. Speech frames give the covariance of
//! speech plus noise, background frames that of noise; their difference is the
//! speech's. No model and no training: the filter is solved from the
//! recording's own statistics.

use crate::stft::Spectrum;
use nalgebra::{DMatrix, DVector, SymmetricEigen};
use rustfft::num_complex::Complex;

type C = Complex<f64>;

/// Per frequency bin, the mics' covariance during speech and during background.
pub struct Covariances {
    speech: Vec<DMatrix<C>>,
    noise: Vec<DMatrix<C>>,
    pub speech_frames: usize,
    pub noise_frames: usize,
}

impl Covariances {
    /// From each mic's spectra (all the same frames): `kind(frame)` says speech
    /// (`Some(true)`), background (`Some(false)`) or neither, left out.
    pub fn estimate(spectra: &[Vec<Spectrum>], kind: impl Fn(usize) -> Option<bool>) -> Self {
        let mics = spectra.len();
        let bins = spectra[0][0].len();
        let frames = spectra.iter().map(Vec::len).min().unwrap_or(0);
        let zero = DMatrix::<C>::zeros(mics, mics);
        let (mut speech, mut noise) = (vec![zero.clone(); bins], vec![zero; bins]);
        let (mut ns, mut nn) = (0usize, 0usize);
        for f in 0..frames {
            let into = match kind(f) {
                Some(true) => {
                    ns += 1;
                    &mut speech
                }
                Some(false) => {
                    nn += 1;
                    &mut noise
                }
                None => continue,
            };
            for (k, cov) in into.iter_mut().enumerate() {
                let y = DVector::<C>::from_iterator(mics, spectra.iter().map(|s| s[f][k]));
                *cov += &y * y.adjoint();
            }
        }
        for cov in &mut speech {
            *cov /= C::new(ns.max(1) as f64, 0.0);
        }
        for cov in &mut noise {
            *cov /= C::new(nn.max(1) as f64, 0.0);
        }
        Self {
            speech,
            noise,
            speech_frames: ns,
            noise_frames: nn,
        }
    }
}

/// The filtered spectra: per bin, `w = (Φx + Φn)⁻¹ Φx e_ref` with Φx the
/// speech covariance (speech-time minus background, negative parts removed),
/// applied to every frame.
pub fn filter(spectra: &[Vec<Spectrum>], cov: &Covariances, reference: usize) -> Vec<Spectrum> {
    let mics = spectra.len();
    let bins = spectra[0][0].len();
    let frames = spectra.iter().map(Vec::len).min().unwrap_or(0);
    let weights: Vec<DVector<C>> = (0..bins)
        .map(|k| {
            let noise = regularised(&cov.noise[k]);
            let speech = positive_part(&(&cov.speech[k] - &noise));
            let target = speech.column(reference).into_owned();
            (&speech + &noise).lu().solve(&target).unwrap_or_else(|| {
                let mut e = DVector::<C>::zeros(mics);
                e[reference] = C::new(1.0, 0.0);
                e
            })
        })
        .collect();
    (0..frames)
        .map(|f| {
            (0..bins)
                .map(|k| {
                    let y = DVector::<C>::from_iterator(mics, spectra.iter().map(|s| s[f][k]));
                    (weights[k].adjoint() * y)[(0, 0)]
                })
                .collect()
        })
        .collect()
}

/// A tiny diagonal load, so a mic silent in a band cannot make it singular.
fn regularised(m: &DMatrix<C>) -> DMatrix<C> {
    let n = m.nrows();
    let load = (0..n).map(|i| m[(i, i)].re).sum::<f64>() / n as f64 * 1e-6 + 1e-20;
    m + DMatrix::<C>::identity(n, n) * C::new(load, 0.0)
}

/// The Hermitian matrix with its negative eigenvalues set to zero: speech-time
/// minus background can dip below zero where speech is weak.
fn positive_part(m: &DMatrix<C>) -> DMatrix<C> {
    let hermitian = (m + m.adjoint()) * C::new(0.5, 0.0);
    let eig = SymmetricEigen::new(hermitian);
    let values = eig.eigenvalues.map(|v| C::new(v.max(0.0), 0.0));
    &eig.eigenvectors * DMatrix::from_diagonal(&values) * eig.eigenvectors.adjoint()
}
