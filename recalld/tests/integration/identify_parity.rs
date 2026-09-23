//! The port agrees with `recall.identify` — on a corpus the PYTHON produced.
//!
//! ⚠ The expected names and scores in the fixture came out of a real `Store`
//! driven through the real `rematch_speaker_guesses`
//! (`scripts/gen_identify_parity.py`, seed 20260915), not out of a
//! re-implementation of its arithmetic in the generator. That distinction is the
//! whole value: a generator that recomputed the softmax itself would pin what I
//! believe the Python does.
//!
//! ⛔ **THE GENERATOR IS GONE.** `recall.identify` and the script were deleted on
//! 2026-09-17 with the rest of the Mac's enrolment, so this fixture can no longer
//! be REGENERATED — there is no second implementation left to disagree with. It
//! has stopped being a parity check and is now a regression test that pins the
//! Rust to what the Python did on the day it was retired. Keep it for that; do
//! not read a passing run as evidence that two implementations still agree.
//!
//! ⚠ **The vectors are synthetic, and that is a real limitation.** This
//! repository is public and the voiceprints are members of a household, so what
//! this cannot show is that the two agree on real voices. A differential over the
//! live archive is the evidence for that.

use recalld::identify::{Voiceprint, match_one};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    softmax_temperature: f64,
    voiceprints: Vec<Print>,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Print {
    person: String,
    vector: Vec<f64>,
}

#[derive(Deserialize)]
struct Case {
    shape: String,
    embedding: Vec<f64>,
    expected_person: String,
    expected_score: f64,
}

fn fixture() -> Fixture {
    let raw = include_str!("../fixtures/identify-parity.json");
    serde_json::from_str(raw).expect("the parity fixture parses")
}

#[test]
fn every_case_matches_the_python_name_and_score() {
    let f = fixture();
    let prints: Vec<Voiceprint> = f
        .voiceprints
        .iter()
        .map(|p| Voiceprint {
            person: p.person.clone(),
            vector: p.vector.clone(),
        })
        .collect();
    assert!(!f.cases.is_empty(), "an empty corpus proves nothing");

    let mut disagreements = Vec::new();
    for case in &f.cases {
        let got = match_one(&case.embedding, &prints).expect("a guess");
        if got.person != case.expected_person {
            disagreements.push(format!(
                "{}: name {} != {}",
                case.shape, got.person, case.expected_person
            ));
            continue;
        }
        // The Python rounds to six places; so does the port. A difference beyond
        // that is arithmetic drift, not rounding.
        if (got.score - case.expected_score).abs() > 1e-6 {
            disagreements.push(format!(
                "{}: score {} != {}",
                case.shape, got.score, case.expected_score
            ));
        }
    }
    assert!(
        disagreements.is_empty(),
        "{} of {} cases disagree:\n  {}",
        disagreements.len(),
        f.cases.len(),
        disagreements.join("\n  ")
    );
}

/// ⚠ The temperature is two spellings of one number. A change on either side that
/// is not a change on both silently re-scores the whole archive.
#[test]
fn the_softmax_temperature_is_the_pythons() {
    assert!(
        (fixture().softmax_temperature - recalld::identify::SOFTMAX_TEMPERATURE).abs()
            < f64::EPSILON
    );
}

/// Silence embeds to zeros. The Python guards the division with `+ 1e-12`; so
/// does the port, and the answer must be a number rather than a NaN — a NaN
/// compares false against everything and would quietly lose every comparison it
/// entered.
#[test]
fn an_embedding_of_silence_scores_a_number_not_a_nan() {
    let f = fixture();
    let prints: Vec<Voiceprint> = f
        .voiceprints
        .iter()
        .map(|p| Voiceprint {
            person: p.person.clone(),
            vector: p.vector.clone(),
        })
        .collect();
    let zeros = f
        .cases
        .iter()
        .find(|c| c.shape == "all-zeros")
        .expect("the corpus carries the silence case");
    let got = match_one(&zeros.embedding, &prints).expect("a guess");
    assert!(got.score.is_finite(), "got {}", got.score);
}

/// Nobody enrolled means no guess — NOT a guess with a low score. The archive
/// starts empty, and a confident-looking name on the first turn ever recorded
/// would be worse than silence.
#[test]
fn with_nobody_enrolled_there_is_no_guess_at_all() {
    assert!(match_one(&[1.0, 0.0, 0.0], &[]).is_none());
}

/// A span too short to embed comes back NaN. It ties every person, so any name
/// would be the first print's by row order: no guess, like an empty corpus.
#[test]
fn an_embedding_that_is_not_a_number_gets_no_guess() {
    let prints = [
        Voiceprint {
            person: "a".to_owned(),
            vector: vec![1.0, 0.0],
        },
        Voiceprint {
            person: "b".to_owned(),
            vector: vec![0.0, 1.0],
        },
    ];
    assert!(match_one(&[f64::NAN, 0.5], &prints).is_none());
}
