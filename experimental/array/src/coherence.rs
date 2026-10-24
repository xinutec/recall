//! Magnitude-squared coherence between two aligned signals, by Welch's method:
//! per frequency, how much of one is a linear function of the other. Near 1, a
//! steered sum adds the two in step; near 0, adding them only adds noise.

use rustfft::FftPlanner;
use rustfft::num_complex::Complex;

/// The mean coherence of `x` and `y` within each `(low, high)` band in Hz,
/// over windows of `nfft` samples. A room's echo is a linear filter hundreds of
/// milliseconds long: a window shorter than it reads the echo as unrelated.
pub fn band_coherence(
    x: &[f32],
    y: &[f32],
    rate: u32,
    nfft: usize,
    bands: &[(f64, f64)],
) -> Vec<f64> {
    let hop = nfft / 2;
    let len = x.len().min(y.len());
    let window: Vec<f64> = (0..nfft)
        .map(|i| 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / nfft as f64).cos())
        .collect();
    let fft = FftPlanner::<f64>::new().plan_fft_forward(nfft);
    let bins = nfft / 2 + 1;
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
    while start + nfft <= len {
        let (a, b) = (
            spectrum(&x[start..start + nfft]),
            spectrum(&y[start..start + nfft]),
        );
        for k in 0..bins {
            sxx[k] += a[k].norm_sqr();
            syy[k] += b[k].norm_sqr();
            sxy[k] += a[k] * b[k].conj();
        }
        start += hop;
    }
    let hz = |k: usize| k as f64 * f64::from(rate) / nfft as f64;
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
