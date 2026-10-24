//! Dereverberation's parts, proved on signals whose answer is known.

use array::decimate::to_16k;
use array::stft::{istft_with, stft_with};
use array::wpe::{Wpe, dereverberate};

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

fn convolve(x: &[f32], h: &[f32]) -> Vec<f32> {
    let mut y = vec![0.0; x.len()];
    for (k, &hk) in h.iter().enumerate() {
        if hk != 0.0 {
            for i in k..x.len() {
                y[i] += hk * x[i - k];
            }
        }
    }
    y
}

#[test]
fn a_short_frame_stft_reconstructs() {
    let x = noise(16_000, 1);
    let y = istft_with(&stft_with(&x, 512), x.len(), 512);
    let err: Vec<f32> = x[512..15_000]
        .iter()
        .zip(&y[512..15_000])
        .map(|(a, b)| a - b)
        .collect();
    assert!(rms(&err) < 1e-5 * rms(&x), "{}", rms(&err));
}

#[test]
fn decimating_keeps_the_speech_band_and_drops_what_would_alias() {
    let tone = |hz: f64| -> Vec<f32> {
        (0..48_000)
            .map(|i| (std::f64::consts::TAU * hz * f64::from(i) / 48_000.0).sin() as f32)
            .collect()
    };
    let kept = to_16k(&tone(1_000.0));
    let dropped = to_16k(&tone(12_000.0));
    assert_eq!(kept.len(), 16_000);
    assert!(
        (rms(&kept[500..15_500]) - std::f64::consts::FRAC_1_SQRT_2).abs() < 0.01,
        "{}",
        rms(&kept[500..15_500])
    );
    assert!(
        rms(&dropped[500..15_500]) < 0.01,
        "{}",
        rms(&dropped[500..15_500])
    );
}

/// A room at 16 kHz: a direct path, then an exponentially decaying tail.
fn room(seed: u64, delay: usize, tail_ms: f64) -> Vec<f32> {
    let len = (tail_ms * 16.0) as usize;
    let tail = noise(len, seed);
    let mut h = vec![0.0; delay + len];
    h[delay] = 1.0;
    for (i, t) in tail.iter().enumerate().skip(16) {
        h[delay + i] += t * 1.2 * (-(i as f32) / (len as f32 / 5.0)).exp();
    }
    h
}

/// Late echo, measured where it is all there is: in the talker's silences,
/// from 50 ms after each burst ends, against the bursts' own energy. Cut by
/// multichannel prediction, and by more with four mics than with one.
#[test]
fn wpe_cuts_the_late_echo_more_with_more_mics() {
    // Bursts of 0.5 s, silences of 0.25 s.
    let burst = |i: usize| (i / 4_000) % 3 != 2;
    let talker: Vec<f32> = noise(16_000 * 12, 3)
        .into_iter()
        .enumerate()
        .map(|(i, v)| if burst(i) { v } else { 0.0 })
        .collect();
    let mics: Vec<Vec<f32>> = (0..4)
        .map(|m| convolve(&talker, &room(10 + m, 5 * m as usize, 300.0)))
        .collect();
    // Echo-to-speech ratio in dB: silence energy past 50 ms of each burst's end,
    // over burst energy, away from the edges.
    let echo_db = |x: &[f32]| -> f64 {
        let (mut quiet, mut loud, mut nq, mut nl) = (0.0, 0.0, 0, 0);
        let end = x.len() - 16_000;
        for (i, v) in x.iter().enumerate().take(end).skip(16_000) {
            let e = f64::from(*v).powi(2);
            if burst(i) {
                loud += e;
                nl += 1;
            } else if i % 12_000 >= 8_000 + 800 {
                quiet += e;
                nq += 1;
            }
        }
        10.0 * ((quiet / f64::from(nq)) / (loud / f64::from(nl))).log10()
    };
    let before = echo_db(&mics[0]);
    let run = |n: usize| -> f64 {
        let spectra: Vec<_> = mics[..n].iter().map(|m| stft_with(m, 512)).collect();
        let out = dereverberate(
            &spectra,
            Wpe {
                taps: 10,
                delay: 3,
                iterations: 3,
            },
        );
        echo_db(&istft_with(&out[0], mics[0].len(), 512))
    };
    let (one, four) = (before - run(1), before - run(4));
    eprintln!("late echo before {before:.1} dB; cut by one mic {one:.1} dB, by four {four:.1} dB");
    assert!(
        four > 6.0,
        "four mics cut the late echo by {four:.1} dB (one: {one:.1}; before {before:.1} dB)"
    );
    assert!(four > one + 1.0, "four mics {four:.1} dB, one {one:.1} dB");
}
