use audiod::fuse::{fuse, noise_floors};
use audiod::stft::Stft;

/// Two mics hear the same tone; each adds loud noise in its own band. The
/// fused signal must beat BOTH inputs against the clean signal — the whole
/// point of per-band weighting.
#[test]
fn fusion_beats_both_noisy_inputs() {
    let stft = Stft::new(512);
    let n = 32_000;
    let tone = |i: usize, hz: f32| (2.0 * std::f32::consts::PI * hz * i as f32 / 16_000.0).sin();
    let clean: Vec<f32> = (0..n).map(|i| 0.3 * tone(i, 440.0)).collect();
    // Deterministic pseudo-noise, band-limited by construction: mic A is dirty
    // high (5 kHz region), mic B dirty low (100 Hz region).
    let mic_a: Vec<f32> = (0..n)
        .map(|i| clean[i] + 0.2 * tone(i, 5000.0) * tone(i, 4900.0))
        .collect();
    let mic_b: Vec<f32> = (0..n)
        .map(|i| clean[i] + 0.2 * tone(i, 97.0) * tone(i, 101.0))
        .collect();

    let specs = vec![stft.analyse(&mic_a), stft.analyse(&mic_b)];
    let floors: Vec<Vec<f32>> = specs.iter().map(|s| noise_floors(s, 0.1)).collect();
    let fused = stft.synthesise(&fuse(&specs, &floors));

    let err = |x: &[f32]| -> f32 {
        x.iter()
            .zip(&clean)
            .skip(512)
            .take(n - 1024)
            .map(|(a, b)| (a - b) * (a - b))
            .sum()
    };
    let (ea, eb, ef) = (err(&mic_a), err(&mic_b), err(&fused));
    assert!(ef < ea, "fused {ef} not better than mic A {ea}");
    assert!(ef < eb, "fused {ef} not better than mic B {eb}");
}

#[test]
fn silence_fuses_to_silence_without_blowing_up() {
    let stft = Stft::new(512);
    let silent = vec![0.0f32; 8192];
    let specs = vec![stft.analyse(&silent), stft.analyse(&silent)];
    let floors: Vec<Vec<f32>> = specs.iter().map(|s| noise_floors(s, 0.1)).collect();
    let out = stft.synthesise(&fuse(&specs, &floors));
    assert!(out.iter().all(|s| s.abs() < 1e-6));
}

#[test]
fn a_floor_is_a_low_quantile_not_a_minimum() {
    let stft = Stft::new(512);
    // Steady tone with one dropout frame of zeros in the middle.
    let mut samples: Vec<f32> = (0..8192)
        .map(|i| 0.5 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 16_000.0).sin())
        .collect();
    for s in &mut samples[3840..4352] {
        *s = 0.0;
    }
    let spec = stft.analyse(&samples);
    let floors = noise_floors(&spec, 0.1);
    // The 440 Hz bin: its floor must not be the dropout's zero.
    let bin = (440.0f32 / (16_000.0 / 512.0)).round() as usize;
    assert!(floors[bin] > 0.0);
}
