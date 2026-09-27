//! How much of each delivered segment is speech, stored once per blob
//! (docs/architecture.md).
//!
//! The level scanner measures how loud a segment is; this measures whether
//! anyone was talking. Liveness wants "someone is speaking" rather than "bytes
//! arrived"; the room builder's reference level wants speech-bearing segments;
//! and the queue waits for this measurement and refuses a segment measured
//! silent, because a model asked about silence invents text.
//!
//! Bounded batches, one row per blob, like the level scanner, but newest first:
//! liveness and the reference both read recent rows, and oldest-first would
//! leave them waiting hours behind the archive backfill.

use crate::store;
use audiocore::vad::{Detector, Region};
use rusqlite::Connection;
use std::path::Path;

pub use audiocore::vad::UNKNOWN_SECONDS;

/// Measure up to `batch` unmeasured segments, NEWEST first; returns rows written.
/// Room left in the batch goes to measured segments whose regions were never
/// stored (they predate the column), so a new clip is never queued behind the
/// backfill: the transcription queue waits on its measurement.
///
/// The detector is loaded once per batch: construction costs ~2 s against
/// ~0.5 s of detection per clip.
///
/// # Errors
/// Only for database failures. An undecodable blob is a stored row
/// (`UNKNOWN_SECONDS`), not an error: the row is what stops the scanner
/// revisiting it forever.
pub fn scan_once(root: &Path, batch: usize) -> rusqlite::Result<usize> {
    let conn = store::open(root)?;
    let pending = unmeasured(&conn, batch)?;
    // A segment measured silent has no regions; no need to look again.
    conn.execute(
        "UPDATE segment_speech SET regions = '[]'
         WHERE regions IS NULL AND speech_seconds = 0",
        [],
    )?;
    let backfill = unplaced(&conn, batch - pending.len())?;
    if pending.is_empty() && backfill.is_empty() {
        return Ok(0);
    }
    let mut detector = match Detector::load() {
        Ok(detector) => detector,
        Err(err) => {
            // A broken model is a deployment fault, not a silent room: leave the
            // segments unmeasured for a fixed build, rather than writing zeros.
            tracing::error!(%err, "speech detector unavailable; leaving segments unmeasured");
            return Ok(0);
        }
    };
    let mut written = 0;
    for (filename, source) in pending {
        let found = detector.speech_regions(&root.join("ingest").join(&source).join(&filename));
        let seconds = found.as_ref().map_or(UNKNOWN_SECONDS, |regions| {
            regions.iter().map(Region::seconds).sum()
        });
        conn.execute(
            "INSERT OR IGNORE INTO segment_speech
                 (filename, source, speech_seconds, computed_utc, regions)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            (
                &filename,
                &source,
                seconds,
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                regions_json(found.ok().as_deref())?,
            ),
        )?;
        written += 1;
    }
    for (filename, source) in backfill {
        let found = detector.speech_regions(&root.join("ingest").join(&source).join(&filename));
        // Only the regions: the stored total came from the same detector, and
        // the queue has already acted on it.
        conn.execute(
            "UPDATE segment_speech SET regions = ?2 WHERE filename = ?1 AND regions IS NULL",
            (&filename, regions_json(found.ok().as_deref())?),
        )?;
        written += 1;
    }
    Ok(written)
}

fn unmeasured(conn: &Connection, limit: usize) -> rusqlite::Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT s.filename, s.source FROM segments s
         LEFT JOIN segment_speech p ON p.filename = s.filename
         WHERE p.filename IS NULL
         ORDER BY s.start_utc DESC, s.filename DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit as u32], |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect()
}

/// Measured segments whose regions were never stored, newest first.
fn unplaced(conn: &Connection, limit: usize) -> rusqlite::Result<Vec<(String, String)>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT p.filename, p.source FROM segment_speech p
         JOIN segments s ON s.filename = p.filename
         WHERE p.regions IS NULL
         ORDER BY s.start_utc DESC, p.filename DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit as u32], |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect()
}

/// The stored spelling of a blob's regions; JSON `null` for "could not look".
fn regions_json(regions: Option<&[Region]>) -> rusqlite::Result<String> {
    let spans: Option<Vec<[f64; 2]>> =
        regions.map(|found| found.iter().map(|r| [r.start, r.end]).collect());
    serde_json::to_string(&spans).map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))
}

/// Stored regions, or `None` for JSON `null` (could not look) or garbage.
fn parse_regions(json: &str) -> Option<Vec<Region>> {
    let spans: Option<Vec<[f64; 2]>> = serde_json::from_str(json).ok()?;
    Some(
        spans?
            .into_iter()
            .map(|[start, end]| Region { start, end })
            .collect(),
    )
}

/// What the pass found in one blob, for the write-time sweep: how much speech,
/// and where. Each is `None` until measured.
///
/// # Errors
/// On database failure.
pub fn heard(ingest: &Connection, filename: &str) -> rusqlite::Result<crate::quality::Heard> {
    use rusqlite::OptionalExtension as _;
    let row: Option<(f64, Option<String>)> = ingest
        .query_row(
            "SELECT speech_seconds, regions FROM segment_speech WHERE filename = ?1",
            [filename],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((seconds, regions)) = row else {
        return Ok(crate::quality::Heard::default());
    };
    Ok(crate::quality::Heard {
        seconds: Some(seconds),
        regions: regions.as_deref().and_then(parse_regions),
    })
}

/// This source's newest segment that could be someone talking, as its capture
/// stamp.
///
/// ⚠ Only a segment measured as silent disqualifies. Unmeasured and
/// undecodable (`UNKNOWN_SECONDS`) ones still count: the scanner runs behind
/// live audio, and "not looked at yet" is not evidence of silence.
///
/// # Errors
/// On database failure.
pub fn latest_speech_utc(conn: &Connection, source: &str) -> rusqlite::Result<Option<String>> {
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
/// conflate: what the recorder delivered ("is it running?") and what could be
/// someone talking ("is my voice captured audibly?", so a silent room reads
/// idle on purpose).
///
/// Speech rule as in [`latest_speech_utc`].
///
/// # Errors
/// On database failure.
pub fn liveness_by_source(conn: &Connection) -> rusqlite::Result<Vec<(String, String, String)>> {
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
        // No speech-bearing segment: the empty string rather than an invented
        // time. Callers map it to null.
        let speech: Option<String> = r.get(2)?;
        Ok((r.get(0)?, delivered, speech.unwrap_or_default()))
    })?;
    rows.collect()
}
