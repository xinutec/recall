//! Stage D4 (docs/architecture.md): how much of each delivered segment is
//! SPEECH, stored once per blob.
//!
//! D2 measures how LOUD a segment is; this measures whether anyone was
//! talking, which is a different question and the one three consumers actually
//! want. Liveness wants "someone is speaking" rather than "bytes arrived". The
//! quiet review wants evidence before it proposes deleting anything. The room
//! builder wants to prioritise blocks that carry speech. And D3's calibrated
//! rank is PARKED until its reference can be built from real speech instead of
//! `levels::REAL_SPEECH_MARGIN_DB`, which is a loudness proxy standing in for
//! exactly this measurement.
//!
//! Same discipline as the level scanner it mirrors: bounded batches, oldest
//! first, one row per blob ever — a segment's speech is a fact about its bytes.

use crate::store;
use crate::vad::Detector;
use rusqlite::Connection;
use std::path::Path;

/// Recorded when the blob could not be decoded at all. Negative seconds are
/// impossible, which is the point: "we could not look" must never be stored as
/// the 0.0 that means "nobody spoke". A sweep that cannot tell those apart
/// deletes audio it never examined.
pub const UNKNOWN_SECONDS: f64 = -1.0;

pub fn ensure_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS segment_speech (
             filename       TEXT PRIMARY KEY REFERENCES segments (filename),
             source         TEXT NOT NULL,
             speech_seconds REAL NOT NULL,
             computed_utc   TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS segment_speech_source
             ON segment_speech (source, filename);",
    )
}

/// Measure up to `batch` unmeasured segments, oldest first; returns rows written.
///
/// The detector is loaded ONCE for the whole batch. Python paid ~2 s of model
/// construction per clip to run 0.5 s of detection — five hours instead of one
/// across a cleanup pass — and a per-segment load here would buy that back.
///
/// # Errors
/// Only for database failures. An undecodable blob is a stored row
/// (`UNKNOWN_SECONDS`), not an error: the row is what stops the scanner
/// revisiting it forever.
pub fn scan_once(root: &Path, batch: usize) -> rusqlite::Result<usize> {
    let conn = store::open(root)?;
    ensure_schema(&conn)?;
    let pending: Vec<(String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT s.filename, s.source FROM segments s
             LEFT JOIN segment_speech p ON p.filename = s.filename
             WHERE p.filename IS NULL
             ORDER BY s.start_utc, s.filename LIMIT ?1",
        )?;
        let rows = stmt.query_map([batch as u32], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<Result<_, _>>()?
    };
    if pending.is_empty() {
        return Ok(0);
    }
    let mut detector = match Detector::load() {
        Ok(detector) => detector,
        Err(err) => {
            // A broken model is a DEPLOYMENT fault, not a silent room: leave the
            // segments unmeasured so a fixed build measures them, rather than
            // writing zeros that read as "nobody spoke here, ever".
            tracing::error!(%err, "speech detector unavailable; leaving segments unmeasured");
            return Ok(0);
        }
    };
    let mut written = 0;
    for (filename, source) in pending {
        let path = root.join("ingest").join(&source).join(&filename);
        let seconds = detector.speech_seconds(&path).unwrap_or(UNKNOWN_SECONDS);
        conn.execute(
            "INSERT OR IGNORE INTO segment_speech
                 (filename, source, speech_seconds, computed_utc)
             VALUES (?1, ?2, ?3, ?4)",
            (
                &filename,
                &source,
                seconds,
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            ),
        )?;
        written += 1;
    }
    Ok(written)
}

/// This source's newest segment that actually carried speech, as its capture
/// stamp — the honest form of "active" the architecture asks for ("a recent
/// segment WITH SPEECH"), as opposed to a recent segment of silence.
///
/// # Errors
/// On database failure.
pub fn latest_speech_utc(conn: &Connection, source: &str) -> rusqlite::Result<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT MAX(s.start_utc) FROM segments s
         JOIN segment_speech p ON p.filename = s.filename
         WHERE s.source = ?1 AND p.speech_seconds > 0.0",
    )?;
    let found: Option<String> = stmt.query_row([source], |r| r.get(0))?;
    Ok(found)
}
