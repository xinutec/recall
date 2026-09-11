//! Stage E3's missing half: what a finished `transcribe-room` job MEANS.
//!
//! The runner leases a room block, drives the shim, and retires the job with the
//! shim's reply as opaque JSON (`queue::done`). Measured 2026-09-11: **648 jobs
//! done, 9.6 MB of results, and not one line of either language reads them** —
//! the GPU time is spent and the transcripts exist, unreachable.
//!
//! ⚠ **This module interprets and returns; it does NOT write.** Room turns are
//! gated on #1461 accepting the selection they came from, and putting
//! unvalidated transcripts into the system of record is the one thing that
//! cannot be undone by deleting a row — the archive is what the household said.
//! So the interpretation is built, tested and runnable against the real results
//! now, and the write is a separate decision with a separate commit.
//!
//! ⚠ **"Hidden" was considered and rejected as the safe option.** It is not
//! absent: a hidden row is still in `transcript_fts` (maintained in CODE here,
//! not by a trigger), still counted, and still seen by supersession — which is
//! the machinery whose failure overwrites a person's typed correction. A second
//! writer into that span is not a small thing to guess at.

use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;

/// One turn a room block's transcript implies, in the archive's own terms.
#[derive(Debug, Clone, PartialEq)]
pub struct RoomTurn {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub text: String,
    pub language: Option<String>,
    pub confidence: Option<f64>,
    /// The shim's per-word timings, verbatim, or `None` when it sent none.
    pub word_timings: Option<String>,
}

/// Why a stored result yields no turns. All of these are ordinary, not faults:
/// a refusal and a silent block are both things the fleet expects to see.
#[derive(Debug, PartialEq)]
pub enum Barren {
    /// The shim reported failure (`ok: false`). The clip is the problem.
    Refused,
    /// Valid JSON, no segments — a block with nothing said in it.
    NothingSaid,
    /// The stored result is not the shape this understands.
    Unreadable(String),
}

#[derive(Deserialize)]
struct Reply {
    ok: bool,
    result: Option<Outcome>,
}

#[derive(Deserialize)]
struct Outcome {
    language: Option<String>,
    #[serde(default)]
    segments: Vec<Segment>,
}

#[derive(Deserialize)]
struct Segment {
    start: f64,
    end: f64,
    text: String,
    confidence: Option<f64>,
    #[serde(default)]
    words: Option<serde_json::Value>,
}

/// Seconds-from-block-start to an absolute instant.
fn at(block_start: DateTime<Utc>, offset_s: f64) -> DateTime<Utc> {
    block_start + Duration::milliseconds((offset_s * 1000.0).round() as i64)
}

/// Interpret one stored job result as the turns it implies.
///
/// `block_start` comes from the room block's FILENAME, which is the archive's
/// naming contract (`room-YYYYMMDDTHHMMSS.flac`) — the shim's offsets are
/// relative to the clip it was handed and mean nothing on their own.
///
/// ⚠ **A turn with no word in it is dropped here**, not left for a later sweep.
/// Transcribing near-silence does not return nothing, it returns inventions:
/// measured on this very queue, a silent minute came back as "Thank you." twice
/// and another as a 150-character run of tildes (#1410). The queue already
/// refuses MEASURED silence a job; this is the same rule one stage later, for
/// the blocks whose silence nobody had measured yet.
pub fn interpret(block_start: DateTime<Utc>, stored: &str) -> Result<Vec<RoomTurn>, Barren> {
    let reply: Reply =
        serde_json::from_str(stored).map_err(|e| Barren::Unreadable(e.to_string()))?;
    if !reply.ok {
        return Err(Barren::Refused);
    }
    let outcome = reply.result.ok_or(Barren::NothingSaid)?;
    let turns: Vec<RoomTurn> = outcome
        .segments
        .into_iter()
        .filter(|s| s.text.chars().any(char::is_alphanumeric))
        .filter(|s| s.end > s.start)
        .map(|s| RoomTurn {
            start: at(block_start, s.start),
            end: at(block_start, s.end),
            text: s.text.trim().to_owned(),
            language: outcome.language.clone(),
            confidence: s.confidence,
            word_timings: s.words.as_ref().map(std::string::ToString::to_string),
        })
        .collect();
    if turns.is_empty() {
        return Err(Barren::NothingSaid);
    }
    Ok(turns)
}
