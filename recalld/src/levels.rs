//! Stage D2 (docs/architecture.md): per-device calibration, measured from
//! what each microphone actually delivers. Every stored segment gets one row
//! of level evidence — the dB of its envelope's speech quantile (0.9) and
//! floor quantile (0.1) — and the per-device reference the room builder's
//! rank needs is then a QUERY over those rows, not a number typed anywhere.
//!
//! Why this exists at all: an uncalibrated speech-level rank degenerates into
//! "always the most sensitive microphone" — the condenser leads the phones by
//! 21 dB mostly because of the DEVICE, not the room — so selection without
//! this is the fixed choice it was meant to replace
//! (docs/audio-plane.md, "What the gate measured").
//!
//! Runs as recalld's background scanner: decode (ffmpeg, the same binary the
//! Mac's plane spawns), bucket, store. Bounded batches, oldest first, one row
//! per blob ever — a segment's levels are facts about its bytes and never
//! recomputed.

use crate::store;
use audiocore::decode;
use audiocore::envelope::{level_quantile_db, rms_buckets_at};
use rusqlite::Connection;
use std::path::Path;

pub const SPEECH_QUANTILE: f64 = 0.9;
pub const FLOOR_QUANTILE: f64 = 0.1;
/// The bake-off's envelope resolution (docs/audio-plane.md tier 1).
const BUCKET_S: f64 = 0.1;
const RATE: u32 = 16_000;

/// One segment's measured levels.
#[derive(Debug, Clone, PartialEq)]
pub struct Levels {
    pub speech_db: f32,
    pub floor_db: f32,
}

pub fn ensure_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS segment_levels (
             filename     TEXT PRIMARY KEY REFERENCES segments (filename),
             source       TEXT NOT NULL,
             speech_db    REAL NOT NULL,
             floor_db     REAL NOT NULL,
             computed_utc TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS segment_levels_source
             ON segment_levels (source, filename);",
    )
}

/// Decode one blob and measure it — every container through ffmpeg, the one
/// decoder every other consumer of the archive already trusts.
pub fn measure(path: &Path) -> Option<Levels> {
    let pcm = decode::decode_s16(path, RATE)?;
    let envelope = rms_buckets_at(&pcm, RATE, BUCKET_S);
    Some(Levels {
        speech_db: level_quantile_db(&envelope, SPEECH_QUANTILE),
        floor_db: level_quantile_db(&envelope, FLOOR_QUANTILE),
    })
}

/// Measure up to `batch` unmeasured segments, oldest first. Returns how many
/// rows were written. A blob that cannot be decoded is recorded at
/// `NEG_INFINITY` rather than retried forever — absence of a reading is
/// itself a reading, and the row is what stops the scanner revisiting it.
pub fn scan_once(root: &Path, batch: usize) -> rusqlite::Result<usize> {
    let conn = store::open(root)?;
    ensure_schema(&conn)?;
    let pending: Vec<(String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT s.filename, s.source FROM segments s
             LEFT JOIN segment_levels l ON l.filename = s.filename
             WHERE l.filename IS NULL
             ORDER BY s.start_utc, s.filename LIMIT ?1",
        )?;
        let rows = stmt.query_map([batch as u32], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<Result<_, _>>()?
    };
    let mut written = 0;
    for (filename, source) in pending {
        let path = root.join("ingest").join(&source).join(&filename);
        let levels = measure(&path).unwrap_or(Levels {
            speech_db: f32::NEG_INFINITY,
            floor_db: f32::NEG_INFINITY,
        });
        conn.execute(
            "INSERT OR IGNORE INTO segment_levels
                 (filename, source, speech_db, floor_db, computed_utc)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            (
                &filename,
                &source,
                f64::from(levels.speech_db),
                f64::from(levels.floor_db),
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            ),
        )?;
        written += 1;
    }
    Ok(written)
}

/// A segment feeds its source's reference only when its speech quantile
/// clears its own floor by this much — the plausibly-REAL-speech proxy until
/// stage D4's VAD gives the honest gate. Without it a gated phone's quiet
/// segments drag its reference to the noise gate and its calibrated rank
/// explodes: the speech-to-floor trap in a new hat, and exactly what the WER
/// referee caught on 2026-09-05 (room 0.321 vs usb 0.229, pixel9 carrying
/// 25/29 blocks it had no business carrying).
pub const REAL_SPEECH_MARGIN_DB: f64 = 12.0;

/// The per-device reference the rank compares against: the given quantile of
/// this source's measured speech levels over its most recent `window`
/// REAL-SPEECH rows. A low quantile (say 0.05) reads as "the faintest real
/// speech this microphone records" — the calibrate.py measurement, re-derived
/// continuously from delivery instead of measured once by hand.
pub fn speech_reference_db(
    conn: &Connection,
    source: &str,
    quantile: f64,
    window: u32,
) -> rusqlite::Result<Option<f32>> {
    let levels: Vec<f64> = {
        let mut stmt = conn.prepare(
            "SELECT speech_db FROM (
                 SELECT speech_db FROM segment_levels
                 WHERE source = ?1 AND speech_db > -900.0
                   AND speech_db - floor_db > ?3
                 ORDER BY filename DESC LIMIT ?2
             )",
        )?;
        let rows = stmt.query_map((source, window, REAL_SPEECH_MARGIN_DB), |r| r.get(0))?;
        rows.collect::<Result<_, _>>()?
    };
    if levels.is_empty() {
        return Ok(None);
    }
    let mut sorted = levels;
    sorted.sort_by(f64::total_cmp);
    let at = ((sorted.len() - 1) as f64 * quantile) as usize;
    Ok(Some(sorted[at] as f32))
}
