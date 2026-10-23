//! The instrument, proved on signals whose answer is known.

use array::align::{Frame, fit_drift, gcc_phat, gcc_phat_in};
use array::coherence::band_coherence;
use rustfft::FftPlanner;
use rustfft::num_complex::Complex;

/// Deterministic white noise: an LCG, so a failing case reproduces.
fn noise(len: usize, seed: u64) -> Vec<f32> {
    let mut s = seed;
    (0..len)
        .map(|_| {
            s = s
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((s >> 33) as f32 / (1u64 << 31) as f32) - 0.5
        })
        .collect()
}

/// `x` delayed by `d` samples, possibly fractional, by linear interpolation.
fn delayed(x: &[f32], d: f64) -> Vec<f32> {
    (0..x.len())
        .map(|i| {
            let t = i as f64 - d;
            if t < 0.0 || t >= (x.len() - 1) as f64 {
                return 0.0;
            }
            let k = t.floor() as usize;
            let f = (t - k as f64) as f32;
            x[k] * (1.0 - f) + x[k + 1] * f
        })
        .collect()
}

#[test]
fn gcc_phat_finds_a_whole_sample_delay_and_its_sign() {
    let x = noise(48_000, 1);
    for d in [-300.0, 0.0, 1234.0] {
        let y = delayed(&x, d);
        let found = gcc_phat(&x, &y, 2_000).expect("a peak");
        assert!(
            (found.lag - d).abs() < 0.05,
            "delay {d}: found {}",
            found.lag
        );
    }
}

/// `x` delayed by exactly `d` samples at every frequency (circularly), by a
/// phase shift: linear interpolation delays each frequency differently.
fn phase_delayed(x: &[f32], d: f64) -> Vec<f32> {
    let n = x.len();
    let mut planner = FftPlanner::<f64>::new();
    let mut buf: Vec<Complex<f64>> = x.iter().map(|&v| Complex::new(f64::from(v), 0.0)).collect();
    planner.plan_fft_forward(n).process(&mut buf);
    for (k, v) in buf.iter_mut().enumerate() {
        let f = if k <= n / 2 {
            k as f64
        } else {
            k as f64 - n as f64
        };
        let turn = if 2 * k == n {
            0.0
        } else {
            -std::f64::consts::TAU * f * d / n as f64
        };
        *v *= Complex::from_polar(1.0, turn);
    }
    planner.plan_fft_inverse(n).process(&mut buf);
    buf.iter().map(|v| (v.re / n as f64) as f32).collect()
}

#[test]
fn gcc_phat_finds_a_fractional_delay_to_a_tenth_of_a_sample() {
    let x = noise(48_000, 2);
    let y = phase_delayed(&x, 37.4);
    let found = gcc_phat(&x, &y, 200).expect("a peak");
    assert!((found.lag - 37.4).abs() < 0.1, "found {}", found.lag);
}

#[test]
fn gcc_phat_survives_independent_noise_at_each_mic() {
    let x = noise(48_000, 3);
    let y: Vec<f32> = delayed(&x, 500.0)
        .iter()
        .zip(noise(48_000, 4))
        .map(|(s, n)| s + n)
        .collect();
    let found = gcc_phat(&x, &y, 2_000).expect("a peak");
    assert!((found.lag - 500.0).abs() < 0.5, "found {}", found.lag);
}

#[test]
fn unrelated_signals_give_a_weak_peak() {
    let strong = gcc_phat(&noise(48_000, 5), &delayed(&noise(48_000, 5), 10.0), 500).unwrap();
    let weak = gcc_phat(&noise(48_000, 5), &noise(48_000, 6), 500).unwrap();
    assert!(
        weak.strength * 5.0 < strong.strength,
        "{weak:?} vs {strong:?}"
    );
}

#[test]
fn the_fit_recovers_offset_and_drift_and_ignores_a_wild_frame() {
    // 40 ppm drift on a 120-sample offset, one frame locked onto an echo.
    let mut frames: Vec<Frame> = (0..60)
        .map(|i| {
            let at = f64::from(i) * 96_000.0;
            Frame {
                at,
                lag: 120.0 + at * 40e-6,
                strength: 0.5,
            }
        })
        .collect();
    frames[30].lag += 900.0;
    let fit = fit_drift(&frames, 0.1, 48.0).expect("a fit");
    assert!((fit.offset - 120.0).abs() < 0.01, "{fit:?}");
    assert!((fit.ppm - 40.0).abs() < 0.01, "{fit:?}");
    assert_eq!(fit.used, 59);
    assert!(fit.residual_ms < 0.001, "{fit:?}");
}

#[test]
fn coherence_is_one_for_the_same_signal_and_low_for_independent_ones() {
    let x = noise(96_000, 7);
    let same = band_coherence(&x, &x, 48_000, &[(500.0, 2_000.0)]);
    let apart = band_coherence(&x, &noise(96_000, 8), 48_000, &[(500.0, 2_000.0)]);
    assert!(same[0] > 0.99, "{same:?}");
    assert!(apart[0] < 0.05, "{apart:?}");
}

/// A loud hum common to both mics, undelayed, outvotes a quieter delayed voice
/// over every frequency; within the voice's band the voice's delay wins.
#[test]
fn a_band_limit_finds_the_voice_under_a_shared_hum() {
    let voice = noise(48_000, 9);
    let hum: Vec<f32> = (0..48_000)
        .map(|i| 20.0 * (i as f32 * 0.0065).sin())
        .collect();
    let x: Vec<f32> = voice.iter().zip(&hum).map(|(v, h)| v + h).collect();
    let y: Vec<f32> = phase_delayed(&voice, 300.0)
        .iter()
        .zip(&hum)
        .map(|(v, h)| v + h)
        .collect();
    let found = gcc_phat_in(&x, &y, 1_000, (300.0, 4_000.0)).expect("a peak");
    assert!((found.lag - 300.0).abs() < 0.5, "found {}", found.lag);
}
