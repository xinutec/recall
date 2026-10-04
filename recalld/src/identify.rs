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

crate::statements! {
    ENROLLED: Meaning =
        "SELECT s.name, e.vector FROM speakers s
         JOIN speaker_embeddings e ON e.speaker_id = s.id";
    STORE_EMBEDDING: Meaning =
        "INSERT OR REPLACE INTO transcript_embeddings (segment_id, vector) VALUES (?1, ?2)";
}

pub use transcript::voice::{Guess, SOFTMAX_TEMPERATURE, Voiceprint, match_one};

/// Below this change, a re-derived score is not worth a write.
pub const SCORE_EPSILON: f64 = 1e-4;

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
    let mut stmt = ENROLLED.prepare(conn)?;
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
    STORE_EMBEDDING.execute(
        conn,
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
