//! Stage D3 (docs/architecture.md): the room builder. One UTC-aligned minute
//! at a time, choose the microphone that heard the room best — *for that
//! microphone* — and carry its audio whole into a `room` segment the queue
//! can hand to transcription. Selection, never fusion: per-block choice tied
//! the best single microphone exactly in the WER bake-off while every fusion
//! lost or nulled (docs/audio-plane.md, "What the gate measured"), so this
//! reproduces the measured instrument's behaviour — hard cuts at block
//! boundaries included — rather than improving on it unmeasured.
//!
//! Three rules carried from the evidence:
//!
//! - **The rank is calibrated.** A raw speech level ranks the most sensitive
//!   microphone always (the condenser leads the phones by ~21 dB of device,
//!   not distance). Each source's block level is compared against its OWN
//!   faintest-speech reference from stage D2's table, so the question is
//!   "how well is this mic hearing the speaker, for this mic".
//! - **No verdict on partial evidence.** A block whose overlapping segments
//!   are not all measured yet is deferred — no row, retried next pass — never
//!   ranked on whatever happens to be scanned. A source without a usable
//!   reference is unrankable; a block where nothing is rankable is deferred
//!   too, because building it uncalibrated is the degenerate fixed choice
//!   this stage exists to replace.
//! - **Terminal verdicts only are persisted** (`built`, `no-audio`), each
//!   with full provenance: which sources contributed, at what level, who won
//!   and by how much. A source that delivers later than the settling window
//!   is thereby a *recorded* absence, not a silent one.

use crate::levels;
use crate::store;
use audiocore::decode;
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

/// The synthetic source every built block lands under.
pub const ROOM_SOURCE: &str = "room";
/// The block grid: one minute, UTC-aligned.
pub const BLOCK_S: i64 = 60;
/// ASR's input shape — what the room stream exists to feed.
const RATE: u32 = 16_000;

/// A rank in dB **above this device's own reference** — the only unit sources
/// may be compared in. A newtype so a raw, uncalibrated level cannot cross
/// this boundary by accident.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct CalibratedDb(pub f32);

/// Everything the builder needs decided, in one place — tests own the clock
/// and the thresholds; production takes the defaults.
pub struct RoomConfig {
    /// How long after a block's end before it may be judged: covers delivery
    /// latency (the phones' shadow uploads on a `WorkManager` cadence).
    pub settle: Duration,
    /// Blocks judged per pass, oldest first.
    pub batch: usize,
    /// The reference is this quantile of a source's own recent speech levels
    /// — low, so it reads "the faintest speech this microphone records".
    pub reference_quantile: f64,
    /// How many recent rows the reference is drawn from.
    pub reference_window: u32,
    /// Fewer measured rows than this and a source has no reference yet —
    /// unrankable, never defaulted.
    pub min_reference_rows: u32,
}

impl Default for RoomConfig {
    fn default() -> Self {
        Self {
            settle: Duration::minutes(15),
            batch: 30,
            reference_quantile: 0.05,
            reference_window: 2_000,
            min_reference_rows: 50,
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
}

/// What one pass did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BuildSummary {
    pub built: usize,
    pub silent: usize,
    pub deferred: usize,
}

pub fn ensure_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS room_blocks (
             start_utc    TEXT PRIMARY KEY,
             verdict      TEXT NOT NULL,
             winner       TEXT,
             filename     TEXT,
             contributors TEXT NOT NULL,
             built_utc    TEXT NOT NULL
         );",
    )
}

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
            if covers && settled && !judged.contains(&iso(block)) {
                blocks.insert(block);
            }
        }
    }
    Ok(blocks.iter().copied().take(config.batch).collect())
}

/// The segments overlapping one block, with their measured levels.
/// `Err(())`-like via Option: `None` means some overlap is unmeasured yet.
fn block_contributors(
    conn: &Connection,
    config: &RoomConfig,
    block: DateTime<Utc>,
) -> rusqlite::Result<Option<Vec<Contributor>>> {
    let from = iso(block - Duration::seconds(BLOCK_S));
    let to = iso(block + Duration::seconds(BLOCK_S));
    let mut stmt = conn.prepare(
        "SELECT s.source, s.filename, l.speech_db
         FROM segments s
         LEFT JOIN segment_levels l ON l.filename = s.filename
         WHERE s.source != ?1 AND s.start_utc > ?2 AND s.start_utc < ?3
         ORDER BY s.filename",
    )?;
    let rows: Vec<(String, String, Option<f64>)> = stmt
        .query_map((ROOM_SOURCE, &from, &to), |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<Result<_, _>>()?;
    let mut per_source: BTreeMap<String, f32> = BTreeMap::new();
    for (source, _filename, speech_db) in rows {
        let Some(speech_db) = speech_db else {
            return Ok(None); // unmeasured overlap: no verdict on partial evidence
        };
        let db = speech_db as f32;
        per_source
            .entry(source)
            .and_modify(|best| *best = best.max(db))
            .or_insert(db);
    }
    let mut out = Vec::new();
    for (source, speech_db) in per_source {
        let calibrated = reference_db(conn, config, &source)?
            .map(|reference| CalibratedDb(speech_db - reference));
        out.push(Contributor {
            source,
            speech_db,
            calibrated,
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
    let measured: u32 = conn.query_row(
        "SELECT COUNT(*) FROM segment_levels WHERE source = ?1 AND speech_db > -900.0",
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

/// Encode one block of s16le PCM as FLAC via ffmpeg (the fleet image carries
/// it), landing with the ingest plane's own durability order.
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
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO room_blocks
             (start_utc, verdict, winner, filename, contributors, built_utc)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        (
            iso(block),
            verdict,
            winner,
            filename,
            // Propagated, not defaulted: a provenance row claiming "nobody
            // contributed" because serialisation failed would be a lie with
            // the exact shape of a quiet minute.
            serde_json::to_string(contributors)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
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
    levels::ensure_schema(&conn)?;
    ensure_schema(&conn)?;
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
            record_verdict(&conn, block, "no-audio", None, None, &contributors)?;
            summary.silent += 1;
            continue;
        }
        let winner = audible
            .iter()
            .filter_map(|c| c.calibrated.map(|db| (c, db)))
            .max_by(|a, b| a.1.0.total_cmp(&b.1.0));
        let Some((winner, _rank)) = winner else {
            // Present, heard, and nothing rankable: wait for calibration
            // rather than degrade to raw-loudest.
            summary.deferred += 1;
            continue;
        };
        let pcm = decode::window_pcm(
            &root.join("ingest"),
            &winner.source,
            block,
            BLOCK_S as usize,
            RATE,
        );
        if pcm.iter().all(|b| *b == 0) {
            record_verdict(&conn, block, "no-audio", None, None, &contributors)?;
            summary.silent += 1;
            continue;
        }
        let filename = format!("{ROOM_SOURCE}-{}.flac", stamp(block));
        let bytes = match encode_flac(root, &filename, &pcm) {
            Ok(bytes) => bytes,
            Err(err) => {
                tracing::warn!(%err, block = %iso(block), "room: encode failed; retrying next pass");
                summary.deferred += 1;
                continue;
            }
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
        // The blob is durable; make the two rows land together.
        conn.execute_batch("BEGIN")?;
        let stored = store::insert(&conn, &row).and_then(|()| {
            record_verdict(
                &conn,
                block,
                "built",
                Some(&winner.source),
                Some(&filename),
                &contributors,
            )
        });
        match stored {
            Ok(()) => conn.execute_batch("COMMIT")?,
            Err(err) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(err);
            }
        }
        summary.built += 1;
    }
    Ok(summary)
}

/// Was this block already judged? (Read side for tests and, later, the API.)
pub fn verdict_of(conn: &Connection, block_start_utc: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT verdict FROM room_blocks WHERE start_utc = ?1",
        [block_start_utc],
        |r| r.get(0),
    )
    .optional()
}
