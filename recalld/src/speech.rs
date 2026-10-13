//! How much of each delivered segment is speech, and where: one row per blob.
//! Liveness wants "someone is speaking", not "bytes arrived"; the queue refuses
//! a segment measured silent, since a model asked about silence invents text;
//! render drops a silence phrase where no speech was heard.
//!
//! Bounded batches, newest first, so liveness does not wait behind backfill.

use crate::store;
use audiocore::vad::{Detector, Region};
use rusqlite::Connection;
use std::path::Path;
use transcript::render::Speech;

crate::statements! {
    RECORD: Ingest =
        "INSERT OR IGNORE INTO segment_speech
                 (filename, source, speech_seconds, computed_utc, regions)
             VALUES (?1, ?2, ?3, ?4, ?5)";
    UNMEASURED: Ingest =
        "SELECT s.filename, s.source FROM segments s
         LEFT JOIN segment_speech p ON p.filename = s.filename
         WHERE p.filename IS NULL
         ORDER BY s.start_utc DESC, s.filename DESC LIMIT ?1";
    HEARD: Ingest =
        "SELECT speech_seconds, regions FROM segment_speech WHERE filename = ?1";
    LIVENESS: Ingest =
        "SELECT s.source,
                MAX(s.start_utc),
                MAX(CASE WHEN p.filename IS NULL OR p.speech_seconds != 0.0
                         THEN s.start_utc END)
         FROM segments s
         LEFT JOIN segment_speech p ON p.filename = s.filename
         GROUP BY s.source";
}

pub use audiocore::vad::UNKNOWN_SECONDS;

/// Measure up to `batch` unmeasured segments, newest first. Returns rows
/// written. The detector loads once per batch (~2 s, against ~0.5 s a clip).
///
/// # Errors
/// Only for database failures. An undecodable blob is stored as
/// `UNKNOWN_SECONDS`, so it is not revisited.
pub fn scan_once(root: &Path, batch: usize) -> rusqlite::Result<usize> {
    let conn = store::open(root)?;
    let pending = unmeasured(&conn, batch)?;
    if pending.is_empty() {
        return Ok(0);
    }
    let mut detector = match Detector::load() {
        Ok(detector) => detector,
        Err(err) => {
            // A deployment fault, not a silent room: write nothing.
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
        RECORD.execute(
            &conn,
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
    Ok(written)
}

fn unmeasured(conn: &Connection, limit: usize) -> rusqlite::Result<Vec<(String, String)>> {
    let mut stmt = UNMEASURED.prepare(conn)?;
    let rows = stmt.query_map([limit as u32], |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect()
}

/// The stored spelling of a blob's regions; JSON `null` for "could not look".
fn regions_json(regions: Option<&[Region]>) -> rusqlite::Result<String> {
    let spans: Option<Vec<[f64; 2]>> =
        regions.map(|found| found.iter().map(|r| [r.start, r.end]).collect());
    serde_json::to_string(&spans).map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))
}

/// What the pass found in one clip, as render reads it: each part `None`
/// until measured, and seconds `None` too where it could not decode.
/// Regions are stored as JSON `[[start, end], ...]`, or `null` when the clip
/// could not be decoded.
///
/// # Errors
/// On database failure.
pub fn evidence(ingest: &Connection, filename: &str) -> rusqlite::Result<Speech> {
    use rusqlite::OptionalExtension as _;
    let Some((seconds, regions)) = HEARD
        .query_row(ingest, [filename], |r| {
            Ok((r.get::<_, f64>(0)?, r.get::<_, Option<String>>(1)?))
        })
        .optional()?
    else {
        return Ok(Speech::default());
    };
    let regions = regions.and_then(|raw| {
        serde_json::from_str::<Option<Vec<(f64, f64)>>>(&raw)
            .ok()
            .flatten()
    });
    Ok(Speech {
        seconds: (seconds >= 0.0).then_some(seconds),
        regions,
    })
}

/// Every source's newest delivered capture time ("is it running") and newest
/// possible speech ("is a voice captured"): unmeasured and undecodable
/// segments count, since not looked at is not silence.
///
/// # Errors
/// On database failure.
pub fn liveness_by_source(conn: &Connection) -> rusqlite::Result<Vec<(String, String, String)>> {
    let mut stmt = LIVENESS.prepare(conn)?;
    let rows = stmt.query_map([], |r| {
        let delivered: String = r.get(1)?;
        // Empty for none; callers map it to null.
        let speech: Option<String> = r.get(2)?;
        Ok((r.get(0)?, delivered, speech.unwrap_or_default()))
    })?;
    rows.collect()
}
