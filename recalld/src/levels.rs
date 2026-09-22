//! Per-device calibration (docs/architecture.md), measured from
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

/// A bucket this quiet is not a quiet room — it is a source emitting nothing.
///
/// -80 dBFS is three LSB of a 16-bit sample, and a real analogue front end never
/// gets there: measured 2026-09-12, the USB condenser's quietest 0.1 s over 22
/// evening segments never reached it once.
pub const GATE_DB: f32 = -80.0;
/// How long a silent stretch must be before it counts as a gate closing.
///
/// 0.3 s rather than the 0.25 s the investigation used, because this reads the
/// EXISTING 0.1 s envelope rather than raw samples — three buckets is the nearest
/// the stored resolution can express, and inventing a second finer pass to hit a
/// round number would cost a decode per segment for nothing.
const GATE_MIN_BUCKETS: usize = 3;

/// Two LSB of a 16-bit sample: not a quiet room, a source emitting nothing.
const QUIET_LSB: u16 = 2;

/// One segment's measured levels.
#[derive(Debug, Clone, PartialEq)]
pub struct Levels {
    pub speech_db: f32,
    pub floor_db: f32,
    /// Longest unbroken stretch at or under [`QUIET_LSB`], in seconds — the gate
    /// detector that works on stored audio (#1526).
    ///
    /// Near-zero rather than exact-zero because lossy coding fills exact zeros:
    /// the gating speakerphone's 0.69 s runs read as 0.002 s once archived.
    ///
    /// Only meaningful where somebody was speaking — an empty room takes every
    /// microphone to its floor together.
    ///
    /// Recorded as evidence, decided on by nobody: a per-source median of this
    /// lost to the speech-to-floor gap as a gate detector (#1526), because it
    /// needs fifty reference rows and so carries a repaired device's old
    /// signature for weeks. `processed.rs` holds the measure that won.
    pub quiet_run_s: f32,
    /// Fraction of the segment inside a sub-[`GATE_DB`] stretch of
    /// [`GATE_MIN_BUCKETS`] or more.
    ///
    /// NOT a gate detector despite the name: an un-gained phone's pauses sit
    /// under the threshold with no gate anywhere, so every phone reads high here
    /// during speech. `processed.rs` holds the measure that detects gating.
    ///
    /// Never read it as quality either — gating IMPROVES speech-vs-floor, so a
    /// rank on `speech_db` prefers the microphone destroying its own audio.
    pub gated: f32,
}

/// Decode one blob and measure it — every container through ffmpeg, the one
/// decoder every other consumer of the archive already trusts.
pub fn measure(path: &Path) -> Option<Levels> {
    let pcm = decode::decode_s16(path, RATE)?;
    let envelope = rms_buckets_at(&pcm, RATE, BUCKET_S);
    Some(Levels {
        speech_db: level_quantile_db(&envelope, SPEECH_QUANTILE),
        floor_db: level_quantile_db(&envelope, FLOOR_QUANTILE),
        gated: gated_fraction(&envelope),
        // Free: the same bytes, scanned again. No second decode, no ffprobe.
        quiet_run_s: quiet_run_seconds(&pcm, RATE),
    })
}

/// What fraction of an envelope sits inside a silent RUN, not merely silent.
///
/// ⚠ **The run is the whole point.** A quiet room has quiet buckets scattered
/// through it; a gate produces CONSECUTIVE ones, because it stays shut until it
/// hears a voice again. Counting quiet buckets alone would flag every calm
/// evening, which is the false positive that makes a detector unusable.
#[must_use]
pub fn gated_fraction(envelope: &[f32]) -> f32 {
    if envelope.is_empty() {
        return 0.0;
    }
    let mut inside = 0usize;
    let mut run = 0usize;
    for &bucket in envelope {
        if bucket_db(bucket) <= GATE_DB {
            run += 1;
        } else {
            if run >= GATE_MIN_BUCKETS {
                inside += run;
            }
            run = 0;
        }
    }
    if run >= GATE_MIN_BUCKETS {
        inside += run;
    }
    inside as f32 / envelope.len() as f32
}

/// The longest unbroken run at or under [`QUIET_LSB`], in seconds.
///
/// The longest one, not the total: summing short near-silences measures how
/// quiet the room was, whereas a gate holds one stretch shut until the next
/// word. Reads samples, not the 0.1 s envelope — a bucket spanning a gate's edge
/// averages the word beside it and lifts clear of any floor.
#[must_use]
pub fn quiet_run_seconds(pcm: &[u8], rate: u32) -> f32 {
    let (mut best, mut run) = (0usize, 0usize);
    for pair in pcm.as_chunks::<2>().0 {
        // `unsigned_abs`, because `i16::MIN.abs()` overflows.
        if i16::from_le_bytes(*pair).unsigned_abs() <= QUIET_LSB {
            run += 1;
            best = best.max(run);
        } else {
            run = 0;
        }
    }
    best as f32 / rate as f32
}

/// One bucket's RMS as dBFS. Silence is -inf, which compares below any floor.
fn bucket_db(rms: f32) -> f32 {
    if rms <= 0.0 {
        f32::NEG_INFINITY
    } else {
        20.0 * rms.log10()
    }
}

/// Measure up to `batch` unmeasured segments, oldest first. Returns how many
/// rows were written. A blob that cannot be decoded is recorded at
/// `NEG_INFINITY` rather than retried forever — absence of a reading is
/// itself a reading, and the row is what stops the scanner revisiting it.
pub fn scan_once(root: &Path, batch: usize) -> rusqlite::Result<usize> {
    let conn = store::open(root)?;
    let pending: Vec<(String, String)> = {
        let mut stmt = conn.prepare(
            // ⚠ Each nullable statistic is named here, so the scanner
            // BACKFILLS rows measured before that statistic existed. Without it
            // every older row keeps a NULL forever, the room builder (which
            // refuses to rank on partial evidence) defers every block those
            // segments touch, and room building stops dead — a detector that
            // halts the thing it was meant to improve. ADD A COLUMN, ADD IT HERE.
            "SELECT s.filename, s.source FROM segments s
             LEFT JOIN segment_levels l ON l.filename = s.filename
             WHERE l.filename IS NULL OR l.gated IS NULL OR l.quiet_run_s IS NULL
             ORDER BY s.start_utc, s.filename LIMIT ?1",
        )?;
        let rows = stmt.query_map([batch as u32], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<Result<_, _>>()?
    };
    let mut written = 0;
    for (filename, source) in pending {
        let path = root.join("ingest").join(&source).join(&filename);
        // ⚠ A blob that will not decode gets -inf levels and ZEROS for the two
        // run statistics, and the zeros are the dangerous half: each reads as
        // the cleanest possible microphone when nothing was measured at all. The
        // -inf is what marks the row unusable (`speech_db > -900.0` filters it
        // downstream), so the fields must be read TOGETHER — neither run figure
        // can say on its own whether it was ever looked at.
        //
        // ⚠ And they are zeros rather than NULL deliberately: NULL is what the
        // pending query selects on, so an undecodable blob would be re-decoded
        // on every pass for ever.
        let levels = measure(&path).unwrap_or(Levels {
            speech_db: f32::NEG_INFINITY,
            floor_db: f32::NEG_INFINITY,
            gated: 0.0,
            quiet_run_s: 0.0,
        });
        // ⚠ REPLACE, not IGNORE: a backfill pass revisits a row that already
        // exists precisely because one of its statistics is NULL, and IGNORE
        // would drop the very measurement the revisit was for — silently, and
        // forever, since the next pass would find the same row and do the same.
        conn.execute(
            "INSERT OR REPLACE INTO segment_levels
                 (filename, source, speech_db, floor_db, gated, quiet_run_s, computed_utc)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            (
                &filename,
                &source,
                f64::from(levels.speech_db),
                f64::from(levels.floor_db),
                f64::from(levels.gated),
                f64::from(levels.quiet_run_s),
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
// the room builder parked. The gate below asks the detector instead.

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
