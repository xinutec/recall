//! Naming a voice from its embedding (`transcript::voice`), against the
//! voiceprints stored here.

crate::statements! {
    ENROLLED: Meaning =
        "SELECT s.name, e.vector FROM speakers s
         JOIN speaker_embeddings e ON e.speaker_id = s.id";
}

pub use transcript::voice::{Guess, SOFTMAX_TEMPERATURE, Voiceprint, match_one};

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
