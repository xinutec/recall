//! Speech detection (stage D4) through the public API.
//!
//! The speech fixtures are REAL recordings: silero is trained on speech and a
//! sine wave proves nothing about it in either direction.

use recalld::vad::{Detector, RATE, detection_gain, regions_from_probabilities};
use std::path::Path;

#[test]
fn silence_is_not_speech() {
    let mut d = Detector::load().expect("model");
    let quiet = vec![0.0_f32; RATE as usize * 3];
    assert_eq!(d.regions(&quiet).expect("run"), vec![]);
}

#[test]
fn real_speech_is_mostly_speech() {
    // A REAL recording, not a tone: silero is trained on speech and a sine
    // proves nothing about it either way.
    let mut d = Detector::load().expect("model");
    let path = Path::new("../tests/fixtures/speech/dialogue-en.flac");
    if !path.exists() {
        // Deliberate: a public repo carries no audio. See the golden trace below.
        eprintln!("skipping real_speech_is_mostly_speech: fixture absent");
        return;
    }
    let seconds = d.speech_seconds(path).expect("detect");
    // The fixture is 31.5 s of dialogue with pauses: most of it is speech,
    // and a detector reporting nearly all or nearly none is broken.
    assert!(
        seconds > 15.0 && seconds < 31.5,
        "speech seconds {seconds} outside the plausible band for the fixture"
    );
}

#[test]
fn a_second_language_is_not_a_special_case() {
    let mut d = Detector::load().expect("model");
    let path = Path::new("../tests/fixtures/speech/dialogue-nl.flac");
    if !path.exists() {
        // Deliberate: a public repo carries no audio. See the golden trace below.
        eprintln!("skipping a_second_language_is_not_a_special_case: fixture absent");
        return;
    }
    let seconds = d.speech_seconds(path).expect("detect");
    assert!(seconds > 4.0, "Dutch speech read as {seconds}s");
}

#[test]
fn an_undecodable_segment_is_an_error_not_zero_speech() {
    // "We could not look" must never be recorded as "nobody spoke".
    let mut d = Detector::load().expect("model");
    let missing = Path::new("../tests/fixtures/speech/does-not-exist.flac");
    assert!(d.speech_seconds(missing).is_err());
}

#[test]
fn quiet_audio_is_lifted_but_near_silence_is_not_amplified_into_speech() {
    assert!((detection_gain(0.5) - 1.0).abs() < f32::EPSILON);
    assert!((detection_gain(0.05) - 10.0).abs() < 1e-5);
    // Bounded: room tone at 1e-6 would otherwise be lifted 500000x.
    assert!((detection_gain(0.000_001) - 32.0).abs() < 1e-5);
    assert!((detection_gain(0.0) - 1.0).abs() < f32::EPSILON);
}

#[test]
fn a_short_dip_does_not_split_one_region_in_two() {
    // 0.3 sits between the exit and entry thresholds: ambiguous, so it
    // neither ends the region nor counts as silence.
    let mut probs = vec![0.9_f32; 40];
    probs[20] = 0.3;
    assert_eq!(regions_from_probabilities(&probs).len(), 1);
}

#[test]
fn a_blip_shorter_than_the_minimum_is_not_a_region() {
    let mut probs = vec![0.0_f32; 40];
    probs[10] = 0.9; // one 32 ms window, far below MIN_SPEECH_MS
    assert_eq!(regions_from_probabilities(&probs), vec![]);
}

/// ⚠ THE FIXTURE-BACKED TESTS ABOVE CANNOT RUN EVERYWHERE. `recall` is a PUBLIC
/// repo and `.gitignore` refuses audio outright, so the speech fixtures are on
/// this Mac and in no clone, sandbox or CI runner. They skip where the file is
/// absent — which means the real-speech coverage is LOCAL ONLY, and something
/// that runs everywhere has to pin the model's input contract instead.
///
/// This is that guard. The probabilities are a golden trace over deterministic
/// pseudo-noise, and they are sensitive to the exact bug that cost an hour:
/// dropping silero's 64-sample context takes the first window from 0.006360 to
/// 0.001617 (measured by ablation, 2026-09-05). A contract regression therefore
/// fails HERE, loudly, rather than silently reporting an empty room.
#[test]
fn the_model_input_contract_is_pinned_by_a_golden_trace() {
    let mut d = Detector::load().expect("model");
    let samples: Vec<f32> = (0..16_000_u32)
        .map(|i| {
            let x = i.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            ((x >> 16) as f32 / 32_768.0) - 1.0
        })
        .collect();
    let probs = d.probabilities(&samples).expect("probabilities");
    assert_eq!(probs.len(), 31, "31 whole 512-sample windows in one second");
    let expected = [0.006_360_084_f32, 0.002_024_173, 0.005_085_885];
    for (i, want) in expected.iter().enumerate() {
        assert!(
            (probs[i] - want).abs() < 1e-5,
            "window {i}: {} != {want} — the model's input contract changed",
            probs[i]
        );
    }
}
