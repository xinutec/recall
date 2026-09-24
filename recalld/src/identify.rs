//! Naming a voice from its embedding.
//!
//! Pure arithmetic over vectors, so it lives with the profiles rather than in
//! the process holding the model weights.
//!
//! A person's score is the best cosine over their enrolled voiceprints, never the
//! mean: someone recorded on four microphones has four quite different vectors,
//! and averaging them describes nobody. The best-scoring person is the guess.
//!
//! Its confidence is a softmax over the per-person bests rather than the raw
//! cosine: 0.7 against a 0.68 runner-up and 0.7 against a 0.2 mean opposite
//! things.

/// Softmax temperature. The fixture in `tests/integration/identify_parity.rs`
/// pins it.
pub const SOFTMAX_TEMPERATURE: f64 = 0.1;

/// Below this change, a re-derived score is not worth a write.
pub const SCORE_EPSILON: f64 = 1e-4;

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

/// Whether a re-derived guess is worth writing over the stored one.
///
/// A stored name with no score counts as changed: it has never been scored, and
/// stays invisible to every reader that sorts by confidence until it is.
#[must_use]
pub fn worth_writing(stored: Option<(&str, Option<f64>)>, fresh: &Guess) -> bool {
    match stored {
        None => true,
        Some((name, score)) => {
            name != fresh.person || score.is_none_or(|s| (s - fresh.score).abs() > SCORE_EPSILON)
        }
    }
}

// --- reading the enrolled people ---------------------------------------------

/// Every enrolled voiceprint.
///
/// Vectors are JSON arrays in a TEXT column; a row that will not parse is
/// skipped, not defaulted. A zero vector would not be inert: it could become
/// somebody's best match on quiet audio.
///
/// # Errors
/// If the database refuses.
pub fn enrolled(conn: &rusqlite::Connection) -> rusqlite::Result<Vec<Voiceprint>> {
    let mut stmt = conn.prepare(
        "SELECT s.name, e.vector FROM speakers s
         JOIN speaker_embeddings e ON e.speaker_id = s.id",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = Vec::new();
    for row in rows {
        let (person, raw) = row?;
        if let Ok(vector) = serde_json::from_str::<Vec<f64>>(&raw) {
            out.push(Voiceprint { person, vector });
        }
    }
    Ok(out)
}

/// Store a turn's embedding and the name it implies.
///
/// ⚠ The guess goes in `speaker_guess`, never `speaker_label`: the label is the
/// name a person gave, and the read path shows the two differently.
///
/// # Errors
/// If the database refuses.
pub fn record(
    conn: &rusqlite::Connection,
    turn_id: i64,
    embedding: &[f64],
    guess: Option<&Guess>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO transcript_embeddings (segment_id, vector) VALUES (?1, ?2)",
        rusqlite::params![
            turn_id,
            serde_json::to_string(embedding).unwrap_or_default()
        ],
    )?;
    if let Some(guess) = guess {
        crate::turn_store::set_guess(conn, turn_id, &guess.person, guess.score)?;
    }
    Ok(())
}
