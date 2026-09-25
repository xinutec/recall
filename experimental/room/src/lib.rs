//! The room builder, an experiment (#1388): never run in production. It works
//! on a local copy of the fleet's data (see `README.md`).
//!
//! For each UTC-aligned minute, pick one microphone and carry its audio whole
//! into a `room` segment. Selection, never fusion: per-block choice tied the
//! best single microphone in the WER bake-off while every fusion lost, so this
//! reproduces the measured behaviour, hard cuts at block boundaries included.
//!
//! - **The loudest raw speech level wins.** Each contributor's level against its
//!   own faintest-speech reference is recorded as provenance but does not
//!   choose; see the note in [`build_once`].
//! - **No verdict on partial evidence.** A block whose overlapping segments are
//!   not all measured is deferred: no row, retried next pass.
//! - **Only terminal verdicts are persisted** (`built:raw`, `no-audio`,
//!   `sparse`, `all-gated`), each with its contributors, so a source that
//!   delivers after the settling window is a recorded absence, not a silent one.

pub mod levels;
pub mod pieces;
pub mod processed;

use audiocore::decode;
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use recalld::store;
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

/// The synthetic source every built block lands under.
pub use recalld::store::ROOM_SOURCE;
/// The block grid: one minute, UTC-aligned.
pub const BLOCK_S: i64 = 60;
/// ASR's input shape — what the room stream exists to feed.
const RATE: u32 = 16_000;

/// A level in dB above this device's own reference. A newtype so a raw level
/// cannot be passed as a calibrated one.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct CalibratedDb(pub f32);

/// The builder's thresholds: tests set them, production takes the defaults.
pub struct RoomConfig {
    /// How long after a block's end before it may be judged: covers delivery
    /// latency (phones upload on a `WorkManager` cadence).
    pub settle: Duration,
    /// Blocks judged per pass, oldest first.
    pub batch: usize,
    /// The reference is this quantile of a source's own recent speech levels:
    /// low, so it reads as the faintest speech this microphone records.
    pub reference_quantile: f64,
    /// How many recent rows the reference is drawn from.
    pub reference_window: u32,
    /// Fewer measured rows than this and a source has no reference yet —
    /// unrankable, never defaulted.
    pub min_reference_rows: u32,
    /// Only blocks starting in `[from, to)`; `None` takes every block.
    pub window: Option<(DateTime<Utc>, DateTime<Utc>)>,
}

impl Default for RoomConfig {
    fn default() -> Self {
        Self {
            settle: Duration::minutes(15),
            batch: 30,
            reference_quantile: 0.05,
            reference_window: 2_000,
            min_reference_rows: 50,
            window: None,
        }
    }
}

/// One source's part in one block, persisted as provenance.
#[derive(Debug, Clone, Serialize)]
pub struct Contributor {
    pub source: String,
    pub speech_db: f32,
    /// `None` = no usable reference yet: present, heard, unrankable.
    pub calibrated: Option<CalibratedDb>,
    /// Fraction of this source's minute inside a gate (`levels::gated_fraction`).
    pub gated: f32,
    /// This source's median speech-to-floor gap over its recent segments, or
    /// `None` with too little history. Recorded, never acted on, so the gating
    /// rule in `processed.rs` can be judged from `room_blocks.contributors`
    /// before it decides anything.
    pub gap_db: Option<f32>,
}

/// Above this `gated` fraction, a source spent so much of the minute emitting
/// digital silence that it reports its own noise suppression, not the room.
/// The builder does not apply it; see the note in [`build_once`].
///
/// Measured with `ffmpeg silencedetect` over 83 segments: usb, iphone11 and
/// oneplus6t never reached it, and the gating speakerphone never fell below
/// 0.39, so the exact figure is not load-bearing.
pub const GATED_MAX: f32 = 0.20;

/// What one pass did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BuildSummary {
    pub built: usize,
    pub silent: usize,
    pub deferred: usize,
    /// Blocks where every audible source was gating. Counted apart from
    /// `silent`: the room was not quiet, the microphones refused to say so.
    /// Always zero while the gated filter is off.
    pub gated: usize,
    /// Blocks the winner barely recorded — the room may have been talking and
    /// nothing captured enough of it to transcribe.
    pub sparse: usize,
}

/// How much of a block the winner must have recorded for the block to be worth
/// building. The window is zero-filled where nothing was recorded, and Whisper
/// hallucinates on that silence.
///
/// Deliberately low: it refuses only what is plainly broken. Every block records
/// its coverage, so raise the floor from that distribution before the room turn
/// writer goes on.
const MIN_COVERAGE: f32 = 0.5;

fn minute_floor(t: DateTime<Utc>) -> DateTime<Utc> {
    let secs = t.timestamp();
    DateTime::from_timestamp(secs - secs.rem_euclid(BLOCK_S), 0).unwrap_or(t)
}

fn stamp(t: DateTime<Utc>) -> String {
    t.format("%Y%m%dT%H%M%S").to_string()
}

fn iso(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Blocks touched by delivered segments, oldest first, judged none yet and
/// old enough to settle. Derived from the segments table, so an empty minute
/// simply never appears — silence costs no rows.
fn candidate_blocks(
    conn: &Connection,
    config: &RoomConfig,
    now: DateTime<Utc>,
) -> rusqlite::Result<Vec<DateTime<Utc>>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT s.start_utc FROM segments s
         WHERE s.source != ?1
         ORDER BY s.start_utc",
    )?;
    let starts: Vec<String> = stmt
        .query_map([ROOM_SOURCE], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    let mut judged = std::collections::HashSet::new();
    let mut jstmt = conn.prepare("SELECT start_utc FROM room_blocks")?;
    for row in jstmt.query_map([], |r| r.get::<_, String>(0))? {
        judged.insert(row?);
    }
    let mut blocks: std::collections::BTreeSet<DateTime<Utc>> = std::collections::BTreeSet::new();
    for start in starts {
        let Ok(parsed) = DateTime::parse_from_rfc3339(&start) else {
            continue;
        };
        let seg_start = parsed.with_timezone(&Utc);
        // A nominal segment spans [start, start+60): it touches its own grid
        // minute and, unless aligned, the next.
        for block in [
            minute_floor(seg_start),
            minute_floor(seg_start) + Duration::seconds(BLOCK_S),
        ] {
            let covers = seg_start < block + Duration::seconds(BLOCK_S)
                && seg_start + Duration::seconds(BLOCK_S) > block;
            let settled = block + Duration::seconds(BLOCK_S) + config.settle <= now;
            let wanted = config
                .window
                .is_none_or(|(from, to)| block >= from && block < to);
            if covers && settled && wanted && !judged.contains(&iso(block)) {
                blocks.insert(block);
            }
        }
    }
    Ok(blocks.iter().copied().take(config.batch).collect())
}

/// Each source overlapping one block, with its measured levels. `None` means
/// some overlap is not measured yet.
fn block_contributors(
    conn: &Connection,
    config: &RoomConfig,
    block: DateTime<Utc>,
) -> rusqlite::Result<Option<Vec<Contributor>>> {
    let from = iso(block - Duration::seconds(BLOCK_S));
    let to = iso(block + Duration::seconds(BLOCK_S));
    let mut stmt = conn.prepare(
        "SELECT s.source, s.filename, l.speech_db, l.gated
         FROM segments s
         LEFT JOIN segment_levels l ON l.filename = s.filename
         WHERE s.source != ?1 AND s.start_utc > ?2 AND s.start_utc < ?3
         ORDER BY s.filename",
    )?;
    let rows: Vec<(String, String, Option<f64>, Option<f64>)> = stmt
        .query_map((ROOM_SOURCE, &from, &to), |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<Result<_, _>>()?;
    // The loudest reading per source, and the WORST gate reading per source.
    let mut per_source: BTreeMap<String, (f32, f32)> = BTreeMap::new();
    for (source, _filename, speech_db, gated) in rows {
        let Some(speech_db) = speech_db else {
            return Ok(None); // unmeasured overlap: no verdict on partial evidence
        };
        // A NULL gate reading means measured before the detector existed, not
        // clean, so it defers too. `levels::scan_once` backfills these.
        let Some(gated) = gated else {
            return Ok(None);
        };
        let (db, gate) = (speech_db as f32, gated as f32);
        per_source
            .entry(source)
            .and_modify(|(best, worst)| {
                *best = best.max(db);
                *worst = worst.max(gate);
            })
            .or_insert((db, gate));
    }
    let mut out = Vec::new();
    for (source, (speech_db, gated)) in per_source {
        let calibrated = reference_db(conn, config, &source)?
            .map(|reference| CalibratedDb(speech_db - reference));
        let gap_db = crate::processed::source_gap(conn, &source, config.reference_window)?;
        out.push(Contributor {
            source,
            speech_db,
            calibrated,
            gated,
            gap_db,
        });
    }
    Ok(Some(out))
}

/// A source's reference, or `None` while it has too little history to mean
/// anything — in which case the source is unrankable, never defaulted.
fn reference_db(
    conn: &Connection,
    config: &RoomConfig,
    source: &str,
) -> rusqlite::Result<Option<f32>> {
    // Counted through the SAME gate the reference uses, or the threshold would
    // admit a source whose reference is then built from nothing.
    let measured: u32 = conn.query_row(
        "SELECT COUNT(*) FROM segment_levels l
         JOIN segment_speech p ON p.filename = l.filename
         WHERE l.source = ?1 AND l.speech_db > -900.0 AND p.speech_seconds > 0.0",
        [source],
        |r| r.get(0),
    )?;
    if measured < config.min_reference_rows {
        return Ok(None);
    }
    levels::speech_reference_db(
        conn,
        source,
        config.reference_quantile,
        config.reference_window,
    )
}

/// Encode one block of s16le PCM as FLAC via ffmpeg, landing it with the ingest
/// plane's durability order (temp file, fsync, rename, fsync the directory).
fn encode_flac(root: &Path, filename: &str, pcm: &[u8]) -> std::io::Result<Vec<u8>> {
    let dir = root.join("ingest").join(ROOM_SOURCE);
    std::fs::create_dir_all(&dir)?;
    let tmpdir = root.join("ingest").join(".tmp");
    std::fs::create_dir_all(&tmpdir)?;
    let tmp = tmpdir.join(format!("{filename}.encoding"));
    let mut child = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-loglevel", "error"])
        .args([
            "-f",
            "s16le",
            "-ar",
            &RATE.to_string(),
            "-ac",
            "1",
            "-i",
            "-",
        ])
        .args(["-c:a", "flac", "-f", "flac", "-y"])
        .arg(&tmp)
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or_else(|| std::io::Error::other("no ffmpeg stdin"))?
        .write_all(pcm)?;
    let status = child.wait()?;
    if !status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(std::io::Error::other("ffmpeg flac encode failed"));
    }
    let bytes = std::fs::read(&tmp)?;
    {
        let file = std::fs::File::open(&tmp)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, dir.join(filename))?;
    std::fs::File::open(&dir)?.sync_all()?;
    Ok(bytes)
}

fn record_verdict(
    conn: &Connection,
    block: DateTime<Utc>,
    verdict: &str,
    winner: Option<&str>,
    filename: Option<&str>,
    contributors: &[Contributor],
    coverage: Option<f32>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO room_blocks
             (start_utc, verdict, winner, filename, contributors, coverage, built_utc)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        (
            iso(block),
            verdict,
            winner,
            filename,
            // Propagated, not defaulted: an empty list from a failed
            // serialisation would look exactly like a quiet minute.
            serde_json::to_string(contributors)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
            coverage.map(f64::from),
            iso(Utc::now()),
        ),
    )?;
    Ok(())
}

/// Judge up to `config.batch` settled blocks. Deferred blocks write nothing
/// and return next pass.
pub fn build_once(
    root: &Path,
    config: &RoomConfig,
    now: DateTime<Utc>,
) -> rusqlite::Result<BuildSummary> {
    let conn = store::open(root)?;
    let mut summary = BuildSummary::default();
    for block in candidate_blocks(&conn, config, now)? {
        let Some(contributors) = block_contributors(&conn, config, block)? else {
            summary.deferred += 1;
            continue;
        };
        let audible: Vec<&Contributor> = contributors
            .iter()
            .filter(|c| c.speech_db.is_finite())
            .collect();
        if audible.is_empty() {
            // Nothing decodable heard this minute at all.
            record_verdict(&conn, block, "no-audio", None, None, &contributors, None)?;
            summary.silent += 1;
            continue;
        }
        // ⚠ Raw level chooses; the calibrated level stays in provenance. The two
        // ranks disagree on about half of rankable blocks, moving them off the
        // condenser onto phones, and no ground truth covers the minutes where
        // they differ. Raw has measured parity with best-single (median WER
        // 0.229); calibration has no measurement where it differs. To switch:
        // collect ground truth on minutes where the ranks differ, then run the
        // referee on that window.
        //
        // ⚠ A gating source scores better on raw level (silence between words
        // reads as high SNR), so gating must remove a source before the rank,
        // never be weighed in it.
        //
        // The filter is off. The stored `gated` metric (0.1 s RMS buckets) reads
        // every phone as gating during speech, because an unprocessed phone's
        // pauses fall under -80 dBFS without any gate; applied, it makes
        // selection always pick the condenser. `GATED_MAX` came from a
        // sample-level measure, and the two disagree.
        //
        // The candidate replacement is `processed.rs` (median speech-to-floor
        // gap), also off: every contributor records its `gap_db`, so its
        // decisions can be judged from provenance first. It stays off until
        // there is evidence that dropping a source improves a transcript, and
        // while no device is gating every firing would be a false positive. A
        // per-source median of `quiet_run_s` was tried and lost: it needs fifty
        // reference rows, so it holds a repaired device's old signature for
        // weeks.
        let ungated: Vec<&Contributor> = audible.clone();
        if ungated.is_empty() {
            // Every microphone that heard this minute was gating: picking the
            // least bad would archive a transcript indistinguishable from a good
            // one. Unreachable while the filter above is off.
            record_verdict(&conn, block, "all-gated", None, None, &contributors, None)?;
            summary.gated += 1;
            continue;
        }
        let winner = ungated
            .iter()
            .map(|c| (c, c.speech_db))
            .max_by(|a, b| a.1.total_cmp(&b.1));
        let Some((winner, _rank)) = winner else {
            summary.deferred += 1;
            continue;
        };
        let window = decode::window_covered(
            &root.join("ingest"),
            &winner.source,
            block,
            BLOCK_S as usize,
            RATE,
        );
        let coverage = window.coverage;
        let pcm = window.pcm;
        if pcm.iter().all(|b| *b == 0) {
            record_verdict(
                &conn,
                block,
                "no-audio",
                None,
                None,
                &contributors,
                Some(coverage),
            )?;
            summary.silent += 1;
            continue;
        }
        // The all-zero test above only catches a block where nothing was
        // recorded at all; ten seconds of speech padded with fifty of silence
        // passes it.
        if window.coverage < MIN_COVERAGE {
            record_verdict(
                &conn,
                block,
                "sparse",
                Some(&winner.source),
                None,
                &contributors,
                Some(coverage),
            )?;
            summary.sparse += 1;
            continue;
        }
        if write_block(
            root,
            &conn,
            block,
            &winner.source,
            &pcm,
            &contributors,
            coverage,
        )? {
            summary.built += 1;
        } else {
            summary.deferred += 1;
        }
    }
    Ok(summary)
}

/// Encode the block and store the blob and its verdict together. `false` means
/// the encode failed and the block is worth retrying.
///
/// Both rows go in one transaction: a segments row without its verdict would be
/// re-judged, and a verdict without its row names a blob nothing can find.
fn write_block(
    root: &Path,
    conn: &Connection,
    block: DateTime<Utc>,
    winner: &str,
    pcm: &[u8],
    contributors: &[Contributor],
    coverage: f32,
) -> rusqlite::Result<bool> {
    let filename = format!("{ROOM_SOURCE}-{}.flac", stamp(block));
    let Ok(bytes) = encode_flac(root, &filename, pcm) else {
        tracing::warn!(block = %iso(block), "room: encode failed; retrying next pass");
        return Ok(false);
    };
    let row = store::Row {
        source: ROOM_SOURCE.into(),
        filename: filename.clone(),
        start_utc: iso(block),
        bytes: bytes.len() as u64,
        sha256: hex::encode(Sha256::digest(&bytes)),
        received_utc: iso(Utc::now()),
        sent_utc: None,
    };
    conn.execute_batch("BEGIN")?;
    let stored = store::insert(conn, &row).and_then(|()| {
        record_verdict(
            conn,
            block,
            // The verdict names the rule that chose, so a later census can
            // separate blocks by rule without re-deriving old references.
            "built:raw",
            Some(winner),
            Some(&filename),
            contributors,
            Some(coverage),
        )
    });
    match stored {
        Ok(()) => conn.execute_batch("COMMIT")?,
        Err(err) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(err);
        }
    }
    Ok(true)
}

/// The verdict recorded for this block, if it has been judged.
pub fn verdict_of(conn: &Connection, block_start_utc: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT verdict FROM room_blocks WHERE start_utc = ?1",
        [block_start_utc],
        |r| r.get(0),
    )
    .optional()
}
