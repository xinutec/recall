//! Delay between two mics, by generalised cross-correlation with phase
//! transform (GCC-PHAT), and one mic's offset and drift fitted over many frames.

use rustfft::FftPlanner;
use rustfft::num_complex::Complex;

/// The best delay found between two frames.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Peak {
    /// Samples by which the second signal lags the first (`y[t] ≈ x[t - lag]`),
    /// interpolated between samples.
    pub lag: f64,
    /// The phase-transformed correlation at the peak: near 1 for one clean
    /// path, near 0 for unrelated signals.
    pub strength: f64,
}

/// The delay of `y` behind `x` within ±`max_lag` samples, over every
/// frequency; `None` for empty input.
pub fn gcc_phat(x: &[f32], y: &[f32], max_lag: usize) -> Option<Peak> {
    gcc_phat_in(x, y, max_lag, (0.0, f64::INFINITY))
}

/// [`gcc_phat`] over the frequencies in `band` (Hz) only. The phase transform
/// weighs every frequency alike, so a room's hum or hiss outside the speech
/// band would otherwise outvote the speech.
pub fn gcc_phat_in(x: &[f32], y: &[f32], max_lag: usize, band: (f64, f64)) -> Option<Peak> {
    if x.is_empty() || y.is_empty() {
        return None;
    }
    let n = (x.len() + y.len()).next_power_of_two();
    let mut planner = FftPlanner::<f64>::new();
    let forward = planner.plan_fft_forward(n);
    // A Hann taper: a frame cut off sharply leaks a loud hum into every
    // frequency, and the speech band with it.
    let spectrum = |s: &[f32]| {
        let taper =
            |i: usize| 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / s.len() as f64).cos();
        let mut buf: Vec<Complex<f64>> = s
            .iter()
            .enumerate()
            .map(|(i, &v)| Complex::new(f64::from(v) * taper(i), 0.0))
            .collect();
        buf.resize(n, Complex::new(0.0, 0.0));
        forward.process(&mut buf);
        buf
    };
    let (sx, sy) = (spectrum(x), spectrum(y));
    let hz = |k: usize| {
        let k = if k <= n / 2 { k } else { n - k };
        k as f64 * crate::RATE / n as f64
    };
    let mut cross: Vec<Complex<f64>> = sy
        .iter()
        .zip(&sx)
        .enumerate()
        .map(|(k, (b, a))| {
            let g = b * a.conj();
            let m = g.norm();
            if m > 1e-12 && hz(k) >= band.0 && hz(k) < band.1 {
                g / m
            } else {
                Complex::new(0.0, 0.0)
            }
        })
        .collect();
    planner.plan_fft_inverse(n).process(&mut cross);
    let at = |k: i64| cross[k.rem_euclid(n as i64) as usize].re / n as f64;
    let max_lag = max_lag.min(n / 2 - 1) as i64;
    let best = (-max_lag..=max_lag).max_by(|&a, &b| at(a).total_cmp(&at(b)))?;
    // The phase transform whitens, so a clean delay's peak is a sinc, whose
    // neighbours give the fraction exactly: sinc(1-δ)/sinc(δ) = δ/(1-δ). A
    // parabola through the same three points is biased toward the sample.
    let (l, c, r) = (at(best - 1), at(best), at(best + 1));
    let shift = if r >= l {
        if c + r > 0.0 {
            r.max(0.0) / (c + r.max(0.0))
        } else {
            0.0
        }
    } else if c + l > 0.0 {
        -l.max(0.0) / (c + l.max(0.0))
    } else {
        0.0
    };
    Some(Peak {
        lag: best as f64 + shift.clamp(-0.5, 0.5),
        strength: c,
    })
}

/// One frame's measurement: where it sits on the reference's timeline, in
/// samples, and what it found.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    pub at: f64,
    pub lag: f64,
    pub strength: f64,
}

/// A mic's clock against the reference: `lag(at) = offset + ppm·1e-6·at`.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Fit {
    /// Samples, at the reference's sample 0.
    pub offset: f64,
    /// Parts per million by which this mic gains samples on the reference: its
    /// clock runs that much fast.
    pub ppm: f64,
    /// Frames that agreed with the line.
    pub used: usize,
    /// RMS distance of those frames from it, in milliseconds.
    pub residual_ms: f64,
}

/// A least-squares line through the frames at least `min_strength` strong,
/// refitted without the ones more than `outlier` samples off it until none
/// are: an echo can win a frame's peak. `None` with fewer than two frames left.
pub fn fit_drift(frames: &[Frame], min_strength: f64, outlier: f64) -> Option<Fit> {
    let mut kept: Vec<Frame> = frames
        .iter()
        .copied()
        .filter(|f| f.strength >= min_strength)
        .collect();
    loop {
        let (offset, slope) = line(&kept)?;
        let off = |f: &Frame| f.lag - (offset + slope * f.at);
        let before = kept.len();
        kept.retain(|f| off(f).abs() <= outlier);
        if kept.len() == before {
            let rms = (kept.iter().map(|f| off(f).powi(2)).sum::<f64>() / kept.len() as f64).sqrt();
            return Some(Fit {
                offset,
                ppm: slope * 1e6,
                used: kept.len(),
                residual_ms: rms / crate::RATE * 1000.0,
            });
        }
    }
}

fn line(frames: &[Frame]) -> Option<(f64, f64)> {
    if frames.len() < 2 {
        return None;
    }
    let n = frames.len() as f64;
    let mx = frames.iter().map(|f| f.at).sum::<f64>() / n;
    let my = frames.iter().map(|f| f.lag).sum::<f64>() / n;
    let sxx: f64 = frames.iter().map(|f| (f.at - mx).powi(2)).sum();
    if sxx <= 0.0 {
        return None;
    }
    let sxy: f64 = frames.iter().map(|f| (f.at - mx) * (f.lag - my)).sum();
    let slope = sxy / sxx;
    Some((my - slope * mx, slope))
}
