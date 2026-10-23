//! Magnitude-squared coherence between two aligned signals, by Welch's method:
//! per frequency, how much of one is a linear function of the other. Near 1, a
//! steered sum adds the two in step; near 0, adding them only adds noise.

use rustfft::FftPlanner;
use rustfft::num_complex::Complex;

const NFFT: usize = 1024;
const HOP: usize = NFFT / 2;

/// The mean coherence of `x` and `y` within each `(low, high)` band in Hz.
pub fn band_coherence(x: &[f32], y: &[f32], rate: u32, bands: &[(f64, f64)]) -> Vec<f64> {
    let len = x.len().min(y.len());
    let window: Vec<f64> = (0..NFFT)
        .map(|i| 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / NFFT as f64).cos())
        .collect();
    let fft = FftPlanner::<f64>::new().plan_fft_forward(NFFT);
    let bins = NFFT / 2 + 1;
    let (mut sxx, mut syy) = (vec![0.0; bins], vec![0.0; bins]);
    let mut sxy = vec![Complex::new(0.0, 0.0); bins];
    let spectrum = |s: &[f32]| {
        let mut buf: Vec<Complex<f64>> = s
            .iter()
            .zip(&window)
            .map(|(&v, w)| Complex::new(f64::from(v) * w, 0.0))
            .collect();
        fft.process(&mut buf);
        buf
    };
    let mut start = 0;
    while start + NFFT <= len {
        let (a, b) = (
            spectrum(&x[start..start + NFFT]),
            spectrum(&y[start..start + NFFT]),
        );
        for k in 0..bins {
            sxx[k] += a[k].norm_sqr();
            syy[k] += b[k].norm_sqr();
            sxy[k] += a[k] * b[k].conj();
        }
        start += HOP;
    }
    let hz = |k: usize| k as f64 * f64::from(rate) / NFFT as f64;
    bands
        .iter()
        .map(|&(low, high)| {
            let picked: Vec<f64> = (0..bins)
                .filter(|&k| hz(k) >= low && hz(k) < high && sxx[k] > 0.0 && syy[k] > 0.0)
                .map(|k| sxy[k].norm_sqr() / (sxx[k] * syy[k]))
                .collect();
            if picked.is_empty() {
                0.0
            } else {
                picked.iter().sum::<f64>() / picked.len() as f64
            }
        })
        .collect()
}
