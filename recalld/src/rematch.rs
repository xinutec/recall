//! Re-derive stored speaker guesses when the voiceprint corpus has grown.
//!
//! A guess is written once, when a turn's embedding is first stored, against
//! whatever voiceprints existed then; this pass re-asks after later enrolments.

use crate::identify::{match_one, worth_writing};
use audiocore::instant::Stamp;
use rusqlite::Connection;

crate::statements! {
    NEWEST_ENROLMENT: Meaning =
        "SELECT MAX(created_utc) FROM speaker_embeddings";
    STALE: Meaning =
        "SELECT t.id, e.vector, t.speaker_guess, t.speaker_score
           FROM transcript_segments t
           JOIN transcript_embeddings e ON e.segment_id = t.id
          WHERE t.speaker_matched_utc IS NULL OR t.speaker_matched_utc < ?1
          ORDER BY t.id DESC
          LIMIT ?2";
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Pass {
    pub examined: usize,
    pub rewritten: usize,
    pub unchanged: usize,
    /// No enrolled voice matched; the stored guess was left alone.
    pub unmatched: usize,
}

struct Stale {
    id: i64,
    vector: Vec<f64>,
    stored: Option<(String, Option<f64>)>,
}

/// The newest enrolment: guesses older than it are stale.
///
/// # Errors
/// If the database refuses.
pub fn newest_enrolment(conn: &Connection) -> rusqlite::Result<Option<String>> {
    NEWEST_ENROLMENT.query_row(conn, [], |r| r.get::<_, Option<String>>(0))
}

/// Re-derive up to `limit` stale guesses. Writes `speaker_guess` only, never a
/// person's `speaker_label`, and stamps every turn examined, or the pass never
/// finishes.
///
/// # Errors
/// If the database refuses.
pub fn run_once(conn: &mut Connection, limit: usize, now: &Stamp) -> rusqlite::Result<Pass> {
    // Stamping against an empty corpus would make the first enrolment look
    // already applied.
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
    let tx = crate::sql::write(conn)?;
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
            crate::turn_store::set_match(&tx, turn.id, &guess.person, guess.score, now)?;
            pass.rewritten += 1;
        } else {
            stamp(&tx, turn.id, now)?;
            pass.unchanged += 1;
        }
    }
    tx.commit()?;
    Ok(pass)
}

fn stamp(conn: &Connection, id: i64, now: &Stamp) -> rusqlite::Result<()> {
    crate::turn_store::stamp_matched(conn, id, now)
}

/// Turns with an embedding whose guess predates the newest enrolment, hidden
/// and superseded ones too: both can still be read.
fn pending(conn: &Connection, newest: &str, limit: usize) -> rusqlite::Result<Vec<Stale>> {
    let mut stmt = STALE.prepare(conn)?;
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
        if let Ok(vector) = serde_json::from_str::<Vec<f64>>(&raw) {
            out.push(Stale { id, vector, stored });
        }
    }
    Ok(out)
}
