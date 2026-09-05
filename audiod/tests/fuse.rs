use audiod::fuse::{PhaseSource, SourceRank, fuse, noise_floors, rank_source, speech_to_floor_db};
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
    let fused = stft.synthesise(&fuse(&specs, &floors, PhaseSource::Reference(0)));

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
    let out = stft.synthesise(&fuse(&specs, &floors, PhaseSource::Reference(0)));
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

/// The contract that keeps fusion coherent: under `PhaseSource::Reference`
/// every fused bin carries the reference channel's phase, whatever the other
/// sources do. Two mics hearing the same speech from different distances have
/// unrelated phase at a given bin, so a per-bin phase donor would splice
/// unrelated phases together; this asserts we never do.
#[test]
fn reference_phase_survives_a_louder_source() {
    let stft = Stft::new(512);
    let n = 16_384;
    let tone = |i: usize, hz: f32| (2.0 * std::f32::consts::PI * hz * i as f32 / 16_000.0).sin();
    // The reference is the quiet one; the other source is louder AND shifted,
    // so it wins the SNR argmax in most bins while disagreeing on phase.
    let reference: Vec<f32> = (0..n).map(|i| 0.1 * tone(i, 440.0)).collect();
    let louder: Vec<f32> = (0..n).map(|i| 0.9 * tone(i + 37, 440.0)).collect();

    let specs = vec![stft.analyse(&reference), stft.analyse(&louder)];
    let floors: Vec<Vec<f32>> = specs.iter().map(|s| noise_floors(s, 0.1)).collect();
    let fused = fuse(&specs, &floors, PhaseSource::Reference(0));

    let mut checked = 0usize;
    for (frame, reference_frame) in fused.iter().zip(&specs[0]) {
        for (bin, reference_bin) in frame.iter().zip(reference_frame) {
            if bin.norm() > 1e-6 && reference_bin.norm() > 1e-6 {
                let angle = (bin / bin.norm()) - (reference_bin / reference_bin.norm());
                assert!(
                    angle.norm() < 1e-3,
                    "fused bin took phase from elsewhere: {angle:?}"
                );
                checked += 1;
            }
        }
    }
    assert!(
        checked > 100,
        "test proved nothing: only {checked} bins had energy"
    );
}

/// The two policies are genuinely different code paths, so a regression that
/// silently ignored the choice would otherwise pass every other test here.
#[test]
fn the_phase_policies_disagree_when_the_sources_do() {
    let stft = Stft::new(512);
    let n = 16_384;
    let tone = |i: usize, hz: f32| (2.0 * std::f32::consts::PI * hz * i as f32 / 16_000.0).sin();
    let reference: Vec<f32> = (0..n).map(|i| 0.1 * tone(i, 440.0)).collect();
    let louder: Vec<f32> = (0..n).map(|i| 0.9 * tone(i + 37, 440.0)).collect();
    let specs = vec![stft.analyse(&reference), stft.analyse(&louder)];
    let floors: Vec<Vec<f32>> = specs.iter().map(|s| noise_floors(s, 0.1)).collect();

    let kept = stft.synthesise(&fuse(&specs, &floors, PhaseSource::Reference(0)));
    let spliced = stft.synthesise(&fuse(&specs, &floors, PhaseSource::HighestSnr));
    let difference: f32 = kept
        .iter()
        .zip(&spliced)
        .map(|(a, b)| (a - b) * (a - b))
        .sum();
    assert!(
        difference > 1e-3,
        "the phase policy made no difference at all"
    );
}

/// A microphone's speech-to-floor ratio must not depend on its gain: the whole
/// point is comparing a quiet condenser against a phone running AGC.
#[test]
fn speech_to_floor_ignores_a_constant_gain() {
    let envelope: Vec<f32> = (0..200)
        .map(|i| if i % 4 == 0 { 0.4 } else { 0.01 })
        .collect();
    let loud: Vec<f32> = envelope.iter().map(|v| v * 8.0).collect();
    let quiet = speech_to_floor_db(&envelope, 0.9, 0.1);
    let amplified = speech_to_floor_db(&loud, 0.9, 0.1);
    assert!(
        (quiet - amplified).abs() < 1e-3,
        "gain changed the ratio: {quiet} vs {amplified}"
    );
    assert!(
        quiet > 20.0,
        "a 40x peak-to-floor should read well above 20 dB"
    );
}

/// The clean mic must win against one that hears the same speech under noise.
#[test]
fn the_cleaner_microphone_wins_the_block() {
    let clean: Vec<f32> = (0..200)
        .map(|i| if i % 4 == 0 { 0.4 } else { 0.002 })
        .collect();
    let noisy: Vec<f32> = (0..200)
        .map(|i| if i % 4 == 0 { 0.4 } else { 0.15 })
        .collect();
    assert!(
        speech_to_floor_db(&clean, 0.9, 0.1) > speech_to_floor_db(&noisy, 0.9, 0.1),
        "the noisy microphone was preferred"
    );
}

/// An absent source never wins, so a block with no audio cannot select it.
#[test]
fn an_empty_envelope_never_wins() {
    assert!(speech_to_floor_db(&[], 0.9, 0.1) < speech_to_floor_db(&[0.1, 0.2], 0.9, 0.1));
}

/// The trap this ranking exists to avoid, measured on the 2026-06-23 window: a
/// phone running noise suppression emits near-silence between words, so its
/// speech-to-floor ratio beats a condenser that hears the speech 21 dB louder
/// but reports honest room tone. Ranking by speech LEVEL must prefer the
/// condenser; ranking by the ratio picks the phone, which is why that policy
/// is kept only to be argued against.
#[test]
fn noise_suppression_fools_the_ratio_but_not_the_level() {
    // Condenser: loud speech, audible room tone throughout.
    let condenser: Vec<f32> = (0..400)
        .map(|i| if i % 4 == 0 { 0.20 } else { 0.010 })
        .collect();
    // Phone: quieter speech, digital silence gated into every gap.
    let phone: Vec<f32> = (0..400)
        .map(|i| if i % 4 == 0 { 0.02 } else { 0.000_02 })
        .collect();

    let level = |e: &[f32]| rank_source(e, SourceRank::SpeechLevel, 0.9, 0.1);
    let ratio = |e: &[f32]| rank_source(e, SourceRank::SpeechToFloor, 0.9, 0.1);

    assert!(
        level(&condenser) > level(&phone),
        "speech level must prefer the microphone that hears the speaker"
    );
    assert!(
        ratio(&phone) > ratio(&condenser),
        "this test is worthless unless the ratio really does prefer the phone"
    );
}
