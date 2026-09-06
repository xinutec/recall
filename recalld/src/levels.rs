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

// ⚠ HISTORY, so the same mistake is not reinvented: there used to be a
// REAL_SPEECH_MARGIN_DB here — a segment fed its source's reference when its
// speech quantile cleared its own floor by 12 dB. That is a LOUDNESS test
// standing in for a SPEECH test, and it failed as one: the WER referee indicted
// it twice (room 0.321 vs usb 0.229, then pixel9 taking 13 of 29 blocks in a
// window where usb was best throughout), which is why the calibrated rank spent
// stage D3 parked. The gate below asks the detector instead.

/// The per-device reference the rank compares against: the given quantile of
/// this source's measured speech levels over its most recent `window` rows that
/// the VAD says CONTAIN SPEECH. A low quantile (say 0.05) reads as "the faintest real
/// speech this microphone records" — the calibrate.py measurement, re-derived
/// continuously from delivery instead of measured once by hand.
pub fn speech_reference_db(
    conn: &Connection,
    source: &str,
    quantile: f64,
    window: u32,
) -> rusqlite::Result<Option<f32>> {
    let levels: Vec<f64> = {
        // ⚠ Gated on the DETECTOR, not on loudness. The reference means "the
        // faintest REAL SPEECH this microphone records", and a loudness proxy
        // for that is what dragged a gated phone's reference down to its noise
        // floor and made its calibrated rank explode.
        let mut stmt = conn.prepare(
            "SELECT speech_db FROM (
                 SELECT l.speech_db FROM segment_levels l
                 JOIN segment_speech p ON p.filename = l.filename
                 WHERE l.source = ?1 AND l.speech_db > -900.0
                   AND p.speech_seconds > 0.0
                 ORDER BY l.filename DESC LIMIT ?2
             )",
        )?;
        let rows = stmt.query_map((source, window), |r| r.get(0))?;
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
