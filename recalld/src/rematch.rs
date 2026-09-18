//! Re-derive stored speaker guesses when the voiceprint corpus has grown.
//!
//! A guess is written once, when a turn's embedding is first stored, against
//! whatever voiceprints existed then. Enrolment continues; nothing else re-asks
//! (#1657).

use crate::identify::{match_one, worth_writing};
use rusqlite::Connection;

/// What one pass did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Pass {
    pub examined: usize,
    pub rewritten: usize,
    pub unchanged: usize,
    /// No enrolled voice matched; the stored guess was left alone.
    pub unmatched: usize,
}

/// One turn waiting to be re-asked.
struct Stale {
    id: i64,
    vector: Vec<f64>,
    stored: Option<(String, Option<f64>)>,
}

/// The newest enrolment, which is what makes older guesses suspect.
///
/// # Errors
/// If the database refuses.
pub fn newest_enrolment(conn: &Connection) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT MAX(created_utc) FROM speaker_embeddings", [], |r| {
        r.get::<_, Option<String>>(0)
    })
}

/// Re-derive up to `limit` stale guesses.
///
/// Writes `speaker_guess` only — `speaker_label` is the name a person gave, and
/// the machine may disagree with its past self but not with them. Stamps every
/// turn it EXAMINES, including unchanged ones, or the pass never finishes.
///
/// # Errors
/// If the database refuses.
pub fn run_once(conn: &mut Connection, limit: usize, now: &str) -> rusqlite::Result<Pass> {
    // Stamping against an empty corpus would mark every turn fresh, so the
    // first real enrolment would look already applied.
    let Some(newest) = newest_enrolment(conn)? else {
        return Ok(Pass::default());
    };
    let enrolled = crate::identify::enrolled(conn)?;
    if enrolled.is_empty() {
        return Ok(Pass::default());
    }

    let stale = pending(conn, &newest, limit)?;
    if stale.is_empty() {
        return Ok(Pass::default());
    }

    let mut pass = Pass::default();
    let tx = conn.transaction()?;
    for turn in stale {
        pass.examined += 1;
        // No match today is not evidence the old name was wrong.
        let Some(guess) = match_one(&turn.vector, &enrolled) else {
            stamp(&tx, turn.id, now)?;
            pass.unmatched += 1;
            continue;
        };
        let stored = turn.stored.as_ref().map(|(n, s)| (n.as_str(), *s));
        if worth_writing(stored, &guess) {
            tx.execute(
                "UPDATE transcript_segments
                    SET speaker_guess = ?1, speaker_score = ?2, speaker_matched_utc = ?3
                  WHERE id = ?4",
                rusqlite::params![guess.person, guess.score, now, turn.id],
            )?;
            pass.rewritten += 1;
        } else {
            stamp(&tx, turn.id, now)?;
            pass.unchanged += 1;
        }
    }
    tx.commit()?;
    Ok(pass)
}

fn stamp(conn: &Connection, id: i64, now: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE transcript_segments SET speaker_matched_utc = ?1 WHERE id = ?2",
        rusqlite::params![now, id],
    )?;
    Ok(())
}

/// Turns with an embedding whose guess predates the newest enrolment.
///
/// Hidden and superseded turns are included: both can still be read, so a stale
/// name on them is the same fault, merely less visible.
fn pending(conn: &Connection, newest: &str, limit: usize) -> rusqlite::Result<Vec<Stale>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, e.vector, t.speaker_guess, t.speaker_score
           FROM transcript_segments t
           JOIN transcript_embeddings e ON e.segment_id = t.id
          WHERE t.speaker_matched_utc IS NULL OR t.speaker_matched_utc < ?1
          ORDER BY t.id DESC
          LIMIT ?2",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![newest, u32::try_from(limit).unwrap_or(u32::MAX)],
        |r| {
            let raw: String = r.get(1)?;
            let name: Option<String> = r.get(2)?;
            let score: Option<f64> = r.get(3)?;
            Ok((r.get::<_, i64>(0)?, raw, name.map(|n| (n, score))))
        },
    )?;
    let mut out = Vec::new();
    for row in rows {
        let (id, raw, stored) = row?;
        // An unparseable vector is skipped, matching `identify::enrolled`.
        if let Ok(vector) = serde_json::from_str::<Vec<f64>>(&raw) {
            out.push(Stale { id, vector, stored });
        }
    }
    Ok(out)
}
