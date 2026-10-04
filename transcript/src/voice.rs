//! Naming a voice from its embedding: pure arithmetic over vectors.
//!
//! A person's score is the best cosine over their enrolled voiceprints, never the
//! mean: someone recorded on four microphones has four quite different vectors,
//! and averaging them describes nobody. The best-scoring person is the guess.
//!
//! Its confidence is a softmax over the per-person bests rather than the raw
//! cosine: 0.7 against a 0.68 runner-up and 0.7 against a 0.2 mean opposite
//! things.

/// Softmax temperature. The fixture in `recalld/tests/integration/identify_parity.rs`
/// pins it.
pub const SOFTMAX_TEMPERATURE: f64 = 0.1;

/// One enrolled voiceprint: whose it is, and the vector.
#[derive(Debug, Clone, PartialEq)]
pub struct Voiceprint {
    pub person: String,
    pub vector: Vec<f64>,
}

/// What a match decided about one turn.
#[derive(Debug, Clone, PartialEq)]
pub struct Guess {
    pub person: String,
    /// The softmax confidence, rounded to six places so a re-derivation compares
    /// equal to the stored score.
    pub score: f64,
}

fn normalise(v: &[f64]) -> Vec<f64> {
    // `+ 1e-12`: a zero vector (an embedding of pure silence) must not divide by
    // zero, or NaN spreads into every comparison.
    let norm = v.iter().map(|x| x * x).sum::<f64>().sqrt() + 1e-12;
    v.iter().map(|x| x / norm).collect()
}

fn cosine(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Name the voice in `embedding`, or `None` when nobody is enrolled.
///
/// Deliberately no threshold: when anyone is enrolled, every embedding gets a
/// guess. On out-of-domain audio a stranger can score 0.95 against an enrolled
/// voice, so no cutoff separates true from false; the score is reported and the
/// reader decides. Hence a guess never goes in `speaker_label`.
#[must_use]
pub fn match_one(embedding: &[f64], voiceprints: &[Voiceprint]) -> Option<Guess> {
    // A NaN ties every person, so the name would be row order: no guess.
    if embedding.is_empty() || voiceprints.is_empty() || !embedding.iter().all(|x| x.is_finite()) {
        return None;
    }
    let e = normalise(embedding);
    // Best cosine per person, in first-seen order: ties go to the first maximum.
    let mut people: Vec<&str> = Vec::new();
    let mut best: Vec<f64> = Vec::new();
    for print in voiceprints {
        if print.vector.len() != embedding.len() {
            continue;
        }
        let sim = cosine(&e, &normalise(&print.vector));
        if let Some(i) = people.iter().position(|p| *p == print.person) {
            best[i] = best[i].max(sim);
        } else {
            people.push(&print.person);
            best.push(sim);
        }
    }
    if people.is_empty() {
        return None;
    }
    let mut top = 0;
    for (i, score) in best.iter().enumerate() {
        if *score > best[top] {
            top = i;
        }
    }
    // Softmax over the per-person bests, max-subtracted for stability.
    let logits: Vec<f64> = best.iter().map(|s| s / SOFTMAX_TEMPERATURE).collect();
    let peak = logits.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let exps: Vec<f64> = logits.iter().map(|l| (l - peak).exp()).collect();
    let total: f64 = exps.iter().sum();
    let confidence = if total > 0.0 { exps[top] / total } else { 0.0 };
    Some(Guess {
        person: people[top].to_owned(),
        // Six places, the stored precision.
        score: (confidence * 1e6).round() / 1e6,
    })
}
