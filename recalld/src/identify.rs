//! Naming a voice from its embedding (`transcript::voice`), against the
//! voiceprints stored here.

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

/// Whether a re-derived guess is worth writing over the stored one. A stored
/// name with no score counts as changed.
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

/// Every enrolled voiceprint. A row that will not parse is skipped, not made a
/// zero vector, which could become somebody's best match on quiet audio.
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

/// Store a turn's embedding and its guess. The guess goes in `speaker_guess`;
/// `speaker_label` is the name a person gave.
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
