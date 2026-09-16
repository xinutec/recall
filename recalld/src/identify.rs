//! Naming a voice from its embedding — ported from `recall.identify`.
//!
//! ⚠ **This decides whose words a turn is attributed to**, so the rule is copied
//! rather than re-derived, down to the temperature. It is pure arithmetic over
//! vectors, which is exactly why it lives on the side that owns the profiles and
//! not inside the process holding the model weights (`shim_voices` says the same
//! from its end).
//!
//! A person's score is the BEST cosine over their enrolled voiceprints, never the
//! mean: someone recorded on four microphones has four quite different vectors,
//! and averaging them describes nobody. The best-scoring person is the guess.
//!
//! Its confidence is a SOFTMAX over the per-person bests rather than the raw
//! cosine — the "vs the others" likelihood. A 0.7 against a 0.68 runner-up and a
//! 0.7 against a 0.2 mean opposite things, and the raw cosine reports them
//! identically.

/// Softmax temperature. ⚠ Shared with `recall.identify._SOFTMAX_TEMPERATURE`; the
/// two spellings of one number, and a test compares them.
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
    /// The softmax confidence, rounded the way the Python rounds it so a
    /// re-derivation on either side compares equal.
    pub score: f64,
}

fn normalise(v: &[f64]) -> Vec<f64> {
    // ⚠ `+ 1e-12`, matching the Python: a zero vector must not divide by zero.
    // It can happen — an embedding of pure silence — and NaN propagates into
    // every comparison rather than losing one of them.
    let norm = v.iter().map(|x| x * x).sum::<f64>().sqrt() + 1e-12;
    v.iter().map(|x| x / norm).collect()
}

fn cosine(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Name the voice in `embedding`, or `None` when nobody is enrolled.
///
/// ⚠ Returns a guess for EVERY embedding when anyone is enrolled — there is no
/// threshold, deliberately. On out-of-domain audio a visitor scores 0.95 against
/// a household member, so no cutoff separates true from false; the score is
/// reported and the reader decides. That is why a guess is never written to
/// `speaker_label` (see `reads::to_out`: a confirmed name and a guess are
/// different columns and the UI shows them differently).
#[must_use]
pub fn match_one(embedding: &[f64], voiceprints: &[Voiceprint]) -> Option<Guess> {
    if embedding.is_empty() || voiceprints.is_empty() {
        return None;
    }
    let e = normalise(embedding);
    // Best cosine per person, in first-seen order so ties resolve the way the
    // Python's `argmax` resolves them — the FIRST maximum, not the last.
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
    // Softmax over the per-person bests, max-subtracted for stability exactly as
    // the Python does it.
    let logits: Vec<f64> = best.iter().map(|s| s / SOFTMAX_TEMPERATURE).collect();
    let peak = logits.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let exps: Vec<f64> = logits.iter().map(|l| (l - peak).exp()).collect();
    let total: f64 = exps.iter().sum();
    let confidence = if total > 0.0 { exps[top] / total } else { 0.0 };
    Some(Guess {
        person: people[top].to_owned(),
        // Six places, matching `round(float(confidence), 6)`.
        score: (confidence * 1e6).round() / 1e6,
    })
}

/// Whether a re-derived guess is worth writing over the stored one.
///
/// ⚠ The `None` score case is NOT "unchanged": a turn with a name and no score
/// has never been scored, and leaving it that way keeps it invisible to every
/// reader that sorts by confidence.
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

/// Every enrolled voiceprint, as `recall.store.speaker_profiles` reads them.
///
/// ⚠ Vectors are stored as a JSON array in a TEXT column; a row that will not
/// parse is SKIPPED rather than defaulted. A zero vector substituted for a
/// corrupt one would not be inert — it would sit at cosine 0 against everyone
/// and quietly become somebody's best match on quiet audio.
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
/// ⚠ **The guess goes in `speaker_guess`, NEVER `speaker_label`.** The label is
/// the name a PERSON gave; a machine writing there would make its own guess
/// indistinguishable from somebody's decision, and the read path shows the two
/// differently for exactly that reason.
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
        conn.execute(
            "UPDATE transcript_segments SET speaker_guess = ?1, speaker_score = ?2
             WHERE id = ?3",
            rusqlite::params![guess.person, guess.score, turn_id],
        )?;
    }
    Ok(())
}
