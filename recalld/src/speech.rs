//! Stage D4 (docs/architecture.md): how much of each delivered segment is
//! SPEECH, stored once per blob.
//!
//! D2 measures how LOUD a segment is; this measures whether anyone was
//! talking, which is a different question and the one three consumers actually
//! want. Liveness wants "someone is speaking" rather than "bytes arrived". The
//! quiet review wants evidence before it proposes deleting anything. The room
//! builder wants to prioritise blocks that carry speech. And the queue refuses
//! to transcribe a segment measured silent, because asking a model about
//! silence returns inventions rather than nothing (#1410).
//!
//! ⚠ It was also meant to un-park D3's calibrated rank, and it did give that
//! reference an honest speech gate — but the rank stayed parked for a different
//! reason: no corpus can test it (#1461). Speech evidence was necessary and not
//! sufficient.
//!
//! Same discipline as the level scanner it mirrors — bounded batches, one row
//! per blob ever, a segment's speech being a fact about its bytes — with one
//! deliberate difference: it scans NEWEST FIRST.
//!
//! Both consumers that matter read RECENT rows. Liveness asks "is anyone
//! talking now"; the calibrated reference asks for a source's recent speech
//! levels. Oldest-first would have made a 15,800-segment archive block both of
//! them for hours behind audio from June. Newest-first makes the scanner useful
//! within a minute and lets the archive backfill behind it — the same
//! newest-first priority the work queue takes (stage E1).

use crate::store;
use audiocore::vad::Detector;
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

/// Measure up to `batch` unmeasured segments, NEWEST first; returns rows written.
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
             ORDER BY s.start_utc DESC, s.filename DESC LIMIT ?1",
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

/// This source's newest segment that could be someone TALKING, as its capture
/// stamp — the honest form of "active" the architecture asks for.
///
/// ⚠ Only a segment MEASURED AS SILENT disqualifies. An unmeasured one still
/// counts, because "not looked at yet" is not evidence of silence — and with a
/// backlog scanning behind live audio, treating unmeasured as silent would
/// black out every recorder the moment this shipped. UNKNOWN (undecodable)
/// counts for the same reason.
///
/// # Errors
/// On database failure.
pub fn latest_speech_utc(conn: &Connection, source: &str) -> rusqlite::Result<Option<String>> {
    // ⚠ The table may not exist: the scanner creates it, and the scanner does
    // not run where the ONNX runtime is unavailable. Without this, liveness
    // would 500 on exactly the deployments least able to afford it.
    ensure_schema(conn)?;
    let mut stmt = conn.prepare(
        "SELECT MAX(s.start_utc) FROM segments s
         LEFT JOIN segment_speech p ON p.filename = s.filename
         WHERE s.source = ?1
           AND (p.filename IS NULL OR p.speech_seconds != 0.0)",
    )?;
    let found: Option<String> = stmt.query_row([source], |r| r.get(0))?;
    Ok(found)
}

/// Every source's newest capture time, in two flavours the panel must not
/// conflate: what the recorder DELIVERED, and what could be someone TALKING.
///
/// They answer different questions. "Is this recorder running?" is operational
/// and wants delivery. "Is my voice being captured audibly?" is about consent
/// and wants speech — a dot the audio can back, so a silent room reads idle on
/// purpose. Serving one number for both is how geb came to read "off" while
/// recording perfectly (#1428): the panel asked the second question and the
/// reader wanted the first.
///
/// Speech rule: only a segment MEASURED AS SILENT disqualifies. Unmeasured and
/// undecodable ones still count, because the scanner runs BEHIND live audio and
/// "not looked at yet" is not evidence of silence.
///
/// # Errors
/// On database failure.
pub fn liveness_by_source(conn: &Connection) -> rusqlite::Result<Vec<(String, String, String)>> {
    ensure_schema(conn)?;
    let mut stmt = conn.prepare(
        "SELECT s.source,
                MAX(s.start_utc),
                MAX(CASE WHEN p.filename IS NULL OR p.speech_seconds != 0.0
                         THEN s.start_utc END)
         FROM segments s
         LEFT JOIN segment_speech p ON p.filename = s.filename
         GROUP BY s.source",
    )?;
    let rows = stmt.query_map([], |r| {
        let delivered: String = r.get(1)?;
        // No speech-bearing segment at all: report the empty string rather than
        // inventing a time, so the caller can tell "nothing heard" from "not
        // asked". Serde would otherwise need a nullable shape for one case.
        let speech: Option<String> = r.get(2)?;
        Ok((r.get(0)?, delivered, speech.unwrap_or_default()))
    })?;
    rows.collect()
}
