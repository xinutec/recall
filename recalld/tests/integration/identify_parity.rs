//! The matcher against a corpus whose expected names and scores came from the
//! Python matcher it ports, run end to end rather than re-derived in a generator.
//!
//! ⚠ The Python and its generator no longer exist, so the fixture cannot be
//! regenerated: this is a regression test, not evidence that two implementations
//! still agree.
//!
//! The vectors are synthetic because the repository is public; agreement on real
//! voices is `identify_differential`'s job.

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
        // Scores are rounded to six places; a difference beyond that is
        // arithmetic drift, not rounding.
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

/// The fixture's temperature is the shipped one: changing it re-scores the whole
/// archive.
#[test]
fn the_softmax_temperature_is_the_pythons() {
    assert!(
        (fixture().softmax_temperature - recalld::identify::SOFTMAX_TEMPERATURE).abs()
            < f64::EPSILON
    );
}

/// Silence embeds to zeros, and the score must still be a number: a NaN compares
/// false against everything and quietly loses every comparison.
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

/// Nobody enrolled means no guess, not a guess with a low score: the archive
/// starts empty, and a name on the first turn ever recorded would be invented.
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
