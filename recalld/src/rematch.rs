//! Re-derive stored speaker guesses when the voiceprint corpus has grown.
//!
//! ⚠ **A guess is written ONCE and was never revisited, and that cost more than
//! anything else measured here.** `diarized.rs` names a turn when its embedding
//! is first stored, against whatever prints existed at that instant. Enrolment
//! goes on — 798 prints over 10 people by 2026-09-18, from a corpus that was
//! much smaller when most turns were written — and nothing re-asked the
//! question. Measured on the fleet, on the same 277 human-labelled turns:
//!
//! ```text
//! what is STORED in speaker_guess vs the human label   0.491
//! the same rule RE-RUN now, leave-one-out              0.913
//! ```
//!
//! Same turns, same arithmetic. The difference is age, not accuracy.
//!
//! ⚠ **There used to be a pass for this** — `recall.identify`'s
//! `rematch_speaker_guesses` — and the port took the arithmetic
//! ([`crate::identify`]) without it. Comments elsewhere still name it as though
//! it exists; this is what replaced it.
//!
//! # Which turns are stale
//!
//! A guess is suspect when it was derived before the newest voiceprint was
//! enrolled. That rule is self-limiting — it empties out and stays empty — and it
//! RE-ARMS ITSELF the moment somebody enrols a voice, which is the only event
//! that can change an answer here.
//!
//! ⚠ It writes `speaker_matched_utc` on every turn it examines, including ones
//! whose answer did not change. A pass that only stamped the rewrites would
//! re-examine every unchanged turn for ever — the shape the ledgered passes
//! already warn about.
//!
//! ⚠ **`speaker_guess` ONLY, never `speaker_label`.** The label is the name a
//! person gave. This is the machine disagreeing with its past self, which is
//! allowed; overwriting somebody's decision is not.

use crate::identify::{match_one, worth_writing};
use rusqlite::Connection;

/// What one pass did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Pass {
    /// Turns looked at.
    pub examined: usize,
    /// Turns whose name or score changed.
    pub rewritten: usize,
    /// Turns the corpus still answers the same way.
    pub unchanged: usize,
    /// Turns no enrolled voice matched at all.
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

/// Re-derive up to `limit` stale guesses. Returns what it did.
///
/// # Errors
/// If the database refuses.
pub fn run_once(conn: &mut Connection, limit: usize, now: &str) -> rusqlite::Result<Pass> {
    let Some(newest) = newest_enrolment(conn)? else {
        // Nobody is enrolled: there is no answer to re-derive, and stamping
        // turns now would mark them fresh against an empty corpus.
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
        // ⚠ On None the stored guess is LEFT ALONE. "No match today" is not
        // evidence the old name was wrong, and blanking it would trade an answer
        // for nothing.
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
/// ⚠ Hidden and superseded turns are INCLUDED. A turn hidden as a loop can be
/// un-hidden, and a superseded one is still read through its lineage — leaving
/// either with a name the corpus no longer supports is the same staleness this
/// exists to end, only less visible.
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
        // A vector that will not parse is SKIPPED, matching `identify::enrolled`
        // — but it is stamped, or the pass would return to it every time.
        if let Ok(vector) = serde_json::from_str::<Vec<f64>>(&raw) {
            out.push(Stale { id, vector, stored });
        }
    }
    Ok(out)
}
