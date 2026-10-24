//! The combiner's parts, each proved on a signal whose answer is known: the
//! long-frame STFT reconstructs, resampling undoes a clock, and the Wiener
//! filter recovers a reference mic's speech from noisy reverberant copies.

use array::mwf::{Covariances, filter};
use array::resample::onto;
use array::stft::{FRAME, centre, istft, stft};

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

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
}

#[test]
fn the_stft_reconstructs_its_input() {
    let x = noise(10 * FRAME, 1);
    let y = istft(&stft(&x), x.len());
    // Away from the edges, where the first and last frames taper.
    let (a, b) = (FRAME, x.len() - FRAME);
    let err: Vec<f32> = x[a..b].iter().zip(&y[a..b]).map(|(p, q)| p - q).collect();
    assert!(rms(&err) < 1e-5 * rms(&x[a..b]), "{}", rms(&err));
}

/// A band-limited signal that can be read exactly at any instant: sines below
/// 8 kHz with fixed phases.
fn tones(t: f64) -> f32 {
    (1..40)
        .map(|k| {
            let f = 190.0 * f64::from(k);
            (std::f64::consts::TAU * f * t / 48_000.0 + f64::from(k) * 1.7).sin()
        })
        .sum::<f64>() as f32
}

#[test]
fn resampling_undoes_an_offset_and_a_drift() {
    // A mic whose sample i holds the reference's instant (i - 300)(1 - 50e-6):
    // it starts 300 samples late and gains 50 ppm.
    let len = 48_000 * 4;
    let reference: Vec<f32> = (0..len).map(|i| tones(i as f64)).collect();
    let mic: Vec<f32> = (0..len)
        .map(|i| tones((i as f64 - 300.0) * (1.0 - 50e-6)))
        .collect();
    let aligned = onto(&mic, |i| 300.0 + i / (1.0 - 50e-6), len);
    let (a, b) = (1_000, len - 2_000);
    let err: Vec<f32> = reference[a..b]
        .iter()
        .zip(&aligned[a..b])
        .map(|(p, q)| p - q)
        .collect();
    assert!(
        rms(&err) < 0.01 * rms(&reference[a..b]),
        "{}",
        rms(&err) / rms(&reference[a..b])
    );
}

/// Convolve with an impulse response.
fn convolve(x: &[f32], h: &[f32]) -> Vec<f32> {
    let mut y = vec![0.0; x.len()];
    for (k, &hk) in h.iter().enumerate() {
        if hk == 0.0 {
            continue;
        }
        for i in k..x.len() {
            y[i] += hk * x[i - k];
        }
    }
    y
}

/// A room: a direct path then an exponentially decaying tail, `ms` long.
fn room(seed: u64, delay: usize, ms: f64) -> Vec<f32> {
    let len = (ms * 48.0) as usize;
    let tail = noise(len, seed);
    let mut h = vec![0.0; delay + len];
    h[delay] = 1.0;
    for (i, t) in tail.iter().enumerate() {
        h[delay + i] += t * 0.6 * (-(i as f32) / (len as f32 / 4.0)).exp();
    }
    h
}

/// Four mics hear one talker through different rooms, each with its own noise.
/// The filter's job: the talker as the reference mic hears it, with less noise.
#[test]
fn the_wiener_filter_recovers_the_reference_speech_and_gains_snr() {
    let rate = 48_000;
    // A "talker": noise bursts with gaps, so there are speech and noise-only frames.
    let mut talker = noise(rate * 20, 3);
    for (i, v) in talker.iter_mut().enumerate() {
        if (i / (rate * 2)) % 2 == 1 {
            *v = 0.0;
        }
    }
    let clean: Vec<Vec<f32>> = (0..4)
        .map(|m| convolve(&talker, &room(10 + m, 40 * m as usize, 60.0)))
        .collect();
    let mics: Vec<Vec<f32>> = clean
        .iter()
        .enumerate()
        .map(|(m, c)| {
            c.iter()
                .zip(noise(c.len(), 100 + m as u64))
                .map(|(s, n)| s + 0.5 * n)
                .collect()
        })
        .collect();
    let spectra: Vec<_> = mics.iter().map(|m| stft(m)).collect();
    // Speech frames are those inside a burst, known here exactly.
    let speech =
        |frame: usize| Some((centre(frame).max(0.0) as usize / (rate * 2)).is_multiple_of(2));
    let cov = Covariances::estimate(&spectra, speech);
    let out = istft(&filter(&spectra, &cov, 0), mics[0].len());
    // In a burst away from its edges: the output is the clean reference plus
    // less noise than the reference mic had.
    let (a, b) = (rate * 8 + FRAME, rate * 10 - FRAME);
    let err_out: Vec<f32> = out[a..b]
        .iter()
        .zip(&clean[0][a..b])
        .map(|(o, c)| o - c)
        .collect();
    let err_ref: Vec<f32> = mics[0][a..b]
        .iter()
        .zip(&clean[0][a..b])
        .map(|(o, c)| o - c)
        .collect();
    let gain_db = 20.0 * (rms(&err_ref) / rms(&err_out)).log10();
    assert!(gain_db > 4.0, "noise reduced by only {gain_db:.1} dB");
    // And it is the reference's speech, not something else: most of the
    // output's speech-time energy is the clean reference.
    assert!(
        rms(&err_out) < 0.5 * rms(&clean[0][a..b]),
        "{}",
        rms(&err_out) / rms(&clean[0][a..b])
    );
}
