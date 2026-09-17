//! Speech detection (stage D4) through the public API.
//!
//! The fixtures are speech, not tones: silero is trained on speech and a sine
//! wave proves nothing about it in either direction. Two kinds, deliberately —
//! `public-domain-en` is a human reading, `dialogue-*` is machine-read invented
//! text (see tests/fixtures/speech/README.md).

use audiocore::vad::{
    Detector, RATE, Splitter, Stream, WINDOW, Windows, detection_gain, regions_from_probabilities,
};
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
        // Not deliberate: .gitignore's blanket *.flac swallowed it (#1433). The
        // golden trace below is what covers the model when this is missing.
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
        // Not deliberate: .gitignore's blanket *.flac swallowed it (#1433). This
        // is the ONLY Dutch coverage in the suite, so a clone loses it entirely.
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
fn quiet_audio_is_lifted_to_the_target_however_quiet_it_is() {
    assert!((detection_gain(0.5) - 1.0).abs() < f32::EPSILON);
    assert!((detection_gain(0.05) - 10.0).abs() < 1e-5);
    // pixel5 across the room: -62 dBFS. A cap at ×32 here is what #1485 was.
    assert!((detection_gain(0.000_79) - 632.911).abs() < 1e-2);
    assert!((detection_gain(0.0) - 1.0).abs() < f32::EPSILON);
}

#[test]
fn one_lsb_of_dither_lifted_to_full_scale_is_still_not_speech() {
    // The fear that bought the old cap, tested at its extreme instead of
    // believed: a file that is digital silence plus one LSB of noise gets the
    // largest lift there is (×16384), and must still read as nobody talking.
    let mut d = Detector::load().expect("model");
    let mut seed: u32 = 0x9E37_79B9;
    let dither: Vec<f32> = (0..RATE as usize * 10)
        .map(|_| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            f32::from((seed >> 30) as i8 - 1) / 32768.0 // -1, 0, 1 LSB, uniform
        })
        .collect();
    let heard: f64 = d
        .regions(&dither)
        .expect("run")
        .iter()
        .map(audiocore::vad::Region::seconds)
        .sum();
    assert!(heard < 0.5, "dither read as {heard:.1}s of speech");
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

/// ⚠ The DIALOGUE fixtures above are absent from a fresh clone — swallowed by
/// .gitignore's blanket `*.flac` — so those tests skip there. The test below
/// uses `public-domain-en.flac`, which IS committed: a reading of Emily
/// Dickinson, public domain worldwide, provenance in
/// `tests/fixtures/speech/README.md`. A HUMAN voice is covered everywhere, which
/// machine-read dialogue would not give on its own (#1433).
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

#[test]
fn committed_public_domain_speech_is_detected_everywhere() {
    // The point of committing a clip: this runs in CI, in the nix sandbox and on
    // a fresh clone. 48 s of read poetry with real pauses between stanzas — so
    // it exercises both halves, speech and the silence around it, on a human
    // voice rather than a synthesised one.
    let mut detector = Detector::load().expect("model");
    let path = Path::new("../tests/fixtures/speech/public-domain-en.flac");
    assert!(path.exists(), "the committed fixture must not vanish");
    let seconds = detector.speech_seconds(path).expect("detect");
    assert!(
        seconds > 25.0 && seconds < 48.0,
        "speech seconds {seconds} outside the plausible band for a 48 s reading"
    );
}

// --- the streaming half: what the live tier reads the tap with ---------------

#[test]
fn a_stream_reproduces_the_batch_probabilities_window_for_window() {
    // The ONE thing the streaming refactor can break: silero carries a state
    // tensor and 64 samples of context between windows, and a stream that
    // resets either still returns plausible numbers — near-zero on obvious
    // speech, which reads as a quiet room rather than as a bug. Feeding the
    // same samples both ways is what makes that visible.
    let samples: Vec<f32> = (0..16_000_u32)
        .map(|i| {
            let x = i.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            ((x >> 16) as f32 / 32_768.0) - 1.0
        })
        .collect();
    let batch = Detector::load()
        .expect("model")
        .probabilities(&samples)
        .expect("probabilities");
    // Gain 1.0 matches what the batch pass derives here: this signal peaks near
    // full scale, so `detection_gain` leaves it alone.
    let mut stream = Stream::open(1.0).expect("model");
    let streamed: Vec<f32> = samples
        .chunks_exact(WINDOW)
        .map(|w| stream.probability(w).expect("window"))
        .collect();
    assert_eq!(streamed.len(), batch.len());
    for (i, (got, want)) in streamed.iter().zip(&batch).enumerate() {
        assert!(
            (got - want).abs() < 1e-6,
            "window {i}: streamed {got} != batch {want} — state is not carried"
        );
    }
}

#[test]
fn a_window_of_the_wrong_length_is_refused_rather_than_answered() {
    // The model's input shape is dynamic, so a short window is accepted and
    // answered with a number. Refusing is the only way that stays visible.
    let mut stream = Stream::open(1.0).expect("model");
    assert!(stream.probability(&[0.0; WINDOW - 1]).is_err());
    assert!(stream.probability(&[0.0; WINDOW + 1]).is_err());
}

#[test]
fn the_last_sentence_is_not_lost_to_the_exit() {
    // Speech still open when the stream ends. Without flush a live agent drops
    // whatever was being said as it shut down.
    let mut splitter = Splitter::new();
    for _ in 0..40 {
        assert_eq!(splitter.push(0.9), None);
    }
    assert_eq!(splitter.flush(), Some(Windows { first: 0, end: 40 }));
    assert_eq!(splitter.flush(), None, "nothing is open twice");
}

#[test]
fn an_unbroken_speaker_can_be_cut_and_the_next_window_starts_the_next_span() {
    // Somebody who never pauses long enough to trigger an end. The hysteresis
    // cannot close that, and a live tier that waits for it is not live.
    let mut splitter = Splitter::new();
    for _ in 0..100 {
        splitter.push(0.9);
    }
    assert_eq!(splitter.open_since(), Some(0));
    assert_eq!(splitter.cut(), Some(Windows { first: 0, end: 100 }));
    assert_eq!(
        splitter.open_since(),
        Some(100),
        "the speaker is still talking"
    );
    for _ in 0..20 {
        splitter.push(0.9);
    }
    assert_eq!(
        splitter.flush(),
        Some(Windows {
            first: 100,
            end: 120
        })
    );
}

#[test]
fn a_cut_with_nobody_talking_is_nothing() {
    let mut splitter = Splitter::new();
    for _ in 0..40 {
        splitter.push(0.0);
    }
    assert_eq!(splitter.cut(), None);
}

#[test]
fn a_span_in_windows_is_the_same_span_in_seconds() {
    // 512 samples at 16 kHz is 32 ms. The live agent slices its buffer by
    // windows and stamps the turn by seconds; they must be the same span.
    let span = Windows { first: 0, end: 100 };
    let region = span.region();
    assert!((region.seconds() - 3.2).abs() < 1e-9);
}
