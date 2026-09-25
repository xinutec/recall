//! Per-device calibration (docs/architecture.md), measured from what each
//! microphone delivers. Every stored segment gets one row of level evidence,
//! the dB of its envelope's speech quantile (0.9) and floor quantile (0.1), and
//! the per-device reference the room builder's rank needs is a query over those
//! rows.
//!
//! Without calibration a speech-level rank always picks the most sensitive
//! microphone: the condenser leads the phones by 21 dB mostly because of the
//! device, not the room (docs/architecture.md, "Decisions that bind").
//!
//! Runs as recalld's background scanner: decode with ffmpeg, bucket, store.
//! Bounded batches, oldest first. A segment's levels are facts about its bytes,
//! so a statistic, once measured, is never recomputed.

use audiocore::decode;
use audiocore::envelope::{level_quantile_db, rms_buckets_at};
use chrono::{DateTime, Utc};
use recalld::store;
use rusqlite::Connection;
use std::path::Path;

pub const SPEECH_QUANTILE: f64 = 0.9;
pub const FLOOR_QUANTILE: f64 = 0.1;
/// The bake-off's envelope resolution (docs/architecture.md).
const BUCKET_S: f64 = 0.1;
const RATE: u32 = 16_000;

/// A bucket this quiet is not a quiet room — it is a source emitting nothing.
///
/// -80 dBFS is three LSB of a 16-bit sample; the USB condenser's quietest 0.1 s
/// over 22 evening segments never reached it.
pub const GATE_DB: f32 = -80.0;
/// How long a silent stretch must be before it counts as a gate closing.
///
/// Three 0.1 s envelope buckets (0.3 s), the nearest the stored envelope comes
/// to a 0.25 s run without a second, finer decode.
const GATE_MIN_BUCKETS: usize = 3;

/// Two LSB of a 16-bit sample: not a quiet room, a source emitting nothing.
const QUIET_LSB: u16 = 2;

/// One segment's measured levels.
#[derive(Debug, Clone, PartialEq)]
pub struct Levels {
    pub speech_db: f32,
    pub floor_db: f32,
    /// Longest unbroken stretch at or under `QUIET_LSB`, in seconds.
    ///
    /// Near-zero rather than exact-zero because lossy coding fills exact zeros:
    /// a gating speakerphone's 0.69 s runs read as 0.002 s once archived. Only
    /// meaningful where somebody was speaking, since an empty room takes every
    /// microphone to its floor.
    ///
    /// Recorded as evidence; nothing decides on it. As a gate detector it needs
    /// fifty reference rows, so it keeps a repaired device's old signature for
    /// weeks. `processed.rs` holds the gate measure.
    pub quiet_run_s: f32,
    /// Fraction of the segment inside a sub-[`GATE_DB`] stretch of
    /// `GATE_MIN_BUCKETS` or more.
    ///
    /// ⚠ Not a gate detector despite the name: an un-gained phone's pauses sit
    /// under the threshold with no gate, so every phone reads high here during
    /// speech. `processed.rs` holds the gate measure.
    ///
    /// Not a quality measure either: gating improves speech-vs-floor, so a rank
    /// on `speech_db` favours the microphone destroying its own audio.
    pub gated: f32,
}

/// Decode one blob with ffmpeg and measure it.
pub fn measure(path: &Path) -> Option<Levels> {
    let pcm = decode::decode_s16(path, RATE)?;
    let envelope = rms_buckets_at(&pcm, RATE, BUCKET_S);
    Some(Levels {
        speech_db: level_quantile_db(&envelope, SPEECH_QUANTILE),
        floor_db: level_quantile_db(&envelope, FLOOR_QUANTILE),
        gated: gated_fraction(&envelope),
        // The same decoded samples, no second decode.
        quiet_run_s: quiet_run_seconds(&pcm, RATE),
    })
}

/// What fraction of an envelope sits inside a silent RUN, not merely silent.
///
/// A quiet room has quiet buckets scattered through it; a gate produces
/// consecutive ones, because it stays shut until it hears a voice. Counting
/// quiet buckets alone would flag every calm evening.
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

/// The longest unbroken run at or under `QUIET_LSB`, in seconds.
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
pub fn scan_once(
    root: &Path,
    batch: usize,
    window: Option<(DateTime<Utc>, DateTime<Utc>)>,
) -> rusqlite::Result<usize> {
    let conn = store::open(root)?;
    let pending: Vec<(String, String)> = {
        let mut stmt = conn.prepare(
            // ⚠ Each nullable statistic is named here so the scanner backfills
            // rows that lack it. A row left NULL makes the room builder, which
            // refuses to rank on partial evidence, defer every block it touches.
            // A new statistic column must be added here.
            "SELECT s.filename, s.source, s.start_utc FROM segments s
             LEFT JOIN segment_levels l ON l.filename = s.filename
             WHERE l.filename IS NULL OR l.gated IS NULL OR l.quiet_run_s IS NULL
             ORDER BY s.start_utc, s.filename",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        // A copy holds blobs for its window only; outside it, a missing blob
        // is not fetched, not unreadable, and must not be measured as silence.
        let mut wanted = Vec::new();
        for row in rows {
            let (filename, source, start) = row?;
            let inside = window.is_none_or(|(from, to)| {
                DateTime::parse_from_rfc3339(&start)
                    .is_ok_and(|t| t.with_timezone(&Utc) >= from && t.with_timezone(&Utc) < to)
            });
            if inside {
                wanted.push((filename, source));
            }
            if wanted.len() >= batch {
                break;
            }
        }
        wanted
    };
    let mut written = 0;
    for (filename, source) in pending {
        let path = root.join("ingest").join(&source).join(&filename);
        // ⚠ An undecodable blob gets -inf levels and zeros for the two run
        // statistics. The zeros read as the cleanest possible microphone; only
        // the -inf (`speech_db > -900.0` downstream) marks the row unusable, so
        // read the fields together. Zeros rather than NULL, because NULL is what
        // the pending query selects on and the blob would be re-decoded forever.
        let levels = measure(&path).unwrap_or(Levels {
            speech_db: f32::NEG_INFINITY,
            floor_db: f32::NEG_INFINITY,
            gated: 0.0,
            quiet_run_s: 0.0,
        });
        // REPLACE, not IGNORE: a backfill revisits an existing row because one
        // of its statistics is NULL, and IGNORE would drop the new measurement.
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

/// The per-device reference the rank compares against: the given quantile of
/// this source's measured speech levels over its most recent `window` rows that
/// the VAD says contain speech. A low quantile (say 0.05) reads as "the faintest
/// real speech this microphone records".
pub fn speech_reference_db(
    conn: &Connection,
    source: &str,
    quantile: f64,
    window: u32,
) -> rusqlite::Result<Option<f32>> {
    let levels: Vec<f64> = {
        // ⚠ Gated on the VAD, not on loudness: a loudness proxy for speech drags
        // a gated phone's reference down to its noise floor.
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
