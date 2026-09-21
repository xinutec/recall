//! The room builder (docs/architecture.md). One UTC-aligned minute
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
//!   faintest-speech reference from the calibration table, so the question is
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
/// What the meaning plane records as its KIND — how a reader tells it from a
/// recorder. `derived` is not a device: it has no microphone to be deaf, no
/// `.alive` marker, and it inherits whichever source won the minute.
pub const ROOM_KIND: &str = "derived";
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
    /// Fraction of this source's minute inside a gate (`levels::gated_fraction`).
    pub gated: f32,
    /// This SOURCE's median speech-to-floor gap over its recent segments, or
    /// `None` with too little history.
    ///
    /// Recorded, never acted on. It is the detector that WON #1526 on
    /// 2026-09-21, and recording it here is what lets the rule be judged before
    /// it ever decides anything: what it would have said is re-derivable from
    /// `room_blocks.contributors` for every block built since.
    pub gap_db: Option<f32>,
}

/// Above this, a source spent so much of the minute emitting digital silence
/// that it is reporting on its own noise suppression, not on the room.
///
/// ⚠ **0.20 is measured, not chosen for roundness.** 83 segments, one per source
/// per day, evenings when the house was occupied: usb, iphone11 and oneplus6t
/// never reached it at all (0%, across 41 segments), geb's BEST segment was 39%
/// and its worst 68%, and pixel5/pixel9 sat near zero with tails to 85-99%. The
/// gap between the cleanest failing case and the dirtiest passing one is wide
/// enough that the exact figure is not load-bearing (#1526).
pub const GATED_MAX: f32 = 0.20;

/// What one pass did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BuildSummary {
    pub built: usize,
    pub silent: usize,
    pub deferred: usize,
    /// Blocks where every audible source was gating. Counted apart from
    /// `silent`: the room was NOT quiet, the microphones refused to say so.
    pub gated: usize,
    /// Blocks the winner barely recorded — the room may have been talking and
    /// nothing captured enough of it to transcribe.
    pub sparse: usize,
}

/// How much of a block a source must have recorded for the block to be worth
/// building. The window is zero-filled where nothing was recorded, and Whisper
/// hallucinates on that silence (#1661).
///
/// Deliberately low and provisional: it refuses only what is plainly broken, and
/// every block records its coverage, so the floor can be raised from the
/// distribution. Raise it before the room turn writer goes on (#1388).
const MIN_COVERAGE: f32 = 0.5;

pub fn ensure_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS room_blocks (
             start_utc    TEXT PRIMARY KEY,
             verdict      TEXT NOT NULL,
             winner       TEXT,
             filename     TEXT,
             contributors TEXT NOT NULL,
             -- NULL means judged before coverage was measured, which is not the
             -- same as fully covered.
             coverage     REAL,
             built_utc    TEXT NOT NULL
         );",
    )?;
    add_coverage_column(conn)
}

/// Add `coverage` to a table that already exists — `CREATE TABLE IF NOT EXISTS`
/// adds nothing to one that is already there.
fn add_coverage_column(conn: &Connection) -> rusqlite::Result<()> {
    let present: bool = conn
        .prepare("SELECT 1 FROM pragma_table_info('room_blocks') WHERE name = 'coverage'")?
        .exists([])?;
    if present {
        return Ok(());
    }
    conn.execute_batch("ALTER TABLE room_blocks ADD COLUMN coverage REAL")
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
        // ⚠ Same rule for the gate reading, and for the same reason: a NULL here
        // is a segment measured before the detector existed, not a segment found
        // to be clean. Ranking on it would be the partial-evidence verdict this
        // function already refuses. `levels::scan_once` backfills these.
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
            // Propagated, not defaulted: a provenance row claiming "nobody
            // contributed" because serialisation failed would be a lie with
            // the exact shape of a quiet minute.
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
    levels::ensure_schema(&conn)?;
    // ⚠ The reference JOINS segment_speech, and the speech scanner is the only
    // thing that creates it — and it declines to run where the ONNX runtime is
    // unavailable. Without this the room builder would fail outright on such a
    // host, which is far worse than building uncalibrated blocks there.
    crate::speech::ensure_schema(&conn)?;
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
            record_verdict(&conn, block, "no-audio", None, None, &contributors, None)?;
            summary.silent += 1;
            continue;
        }
        // ⚠ RAW LEVEL CHOOSES, and calibration stays in provenance. This was
        // un-parked on 2026-09-06 and re-parked the same hour, by measurement:
        //
        //   - The two ranks DISAGREE on 1290 of 2664 rankable blocks (48%), and
        //     the flips are systematic — usb -> iphone11 448, usb -> pixel5 281,
        //     usb -> geb 242, usb -> pixel9 238. Calibration moves blocks off
        //     the condenser onto phones.
        //   - The referee CANNOT test that. Ground truth is mid-June (328 of 468
        //     corrections fall on 14-16 June); the disagreements are September
        //     (1127 of 1290). They overlap on 8 minutes — 1.7%. The corrections
        //     predate the multi-device fleet, so there were barely two mics to
        //     disagree about when they were made.
        //   - The June window that DID pass is therefore no evidence: usb wins
        //     there under both ranks, so it compares identical audio.
        //
        // Raw has MEASURED parity with best-single (median WER 0.229, twice).
        // Calibration has no measurement anywhere it differs. Running the
        // untested rule by default would be a verdict on partial evidence — the
        // rule this file already refuses for a single block, applied to half of
        // them. Nothing consumes room yet, so this costs nothing and keeps the
        // provenance needed to decide later.
        //
        // TO UNPARK: ground truth on SEPTEMBER minutes where the ranks differ
        // (see the census above), then the referee on that window.
        //
        // ⚠ **GATED SOURCES ARE REMOVED BEFORE THE RANK, NOT PENALISED IN IT.**
        // A gate makes a source score BETTER here: deleting everything between
        // words is what produced geb's "52 dB SNR, best in the room" while its
        // transcripts were unusable. So the rank above does not merely fail to
        // notice gating — it actively REWARDS it, and geb won 965 of 1,332
        // transcribed blocks for exactly the reason it should have lost them
        // (#1526). Any weighting scheme would be arguing with a signal that is
        // pointing the wrong way; the only safe move is to drop the source.
        //
        // ⚠ **The `gated` filter is OFF and its measurement stays.** It was
        // switched on briefly and reverted once the backfill produced enough
        // rows to see the fleet-wide distribution.
        //
        // Conditioned on the VAD hearing >= 5 s of speech in the minute:
        //
        //     source      speaking mins   p50 gated   over 0.20
        //     geb                    72        49%        100%
        //     pixel5                494        48%         96%
        //     pixel9                492        45%         92%
        //     iphone11              755        24%         56%
        //     usb                 1,343         0%          0%
        //
        // EVERY PHONE READS AS GATING DURING SPEECH — and on 2026-09-17 that
        // was shown to be the INSTRUMENT, not the phones: a 0.1 s RMS bucket
        // under -80 dBFS is three LSB, and a phone recording UNPROCESSED with a
        // talker across the room peaks at 20 LSB over a 2-3 LSB floor, so its
        // pauses land under the line without any gate. At sample level the same
        // pixel5/pixel9 minutes hold no exact-zero run longer than 0.08 s; geb's
        // speakerphone held 0.69 s runs and 93% zeros. This column separates
        // PHONES FROM THE CONDENSER, and applying it makes selection "always
        // usb": the degenerate fixed choice this whole stage exists to replace.
        //
        // ⚠ **And the threshold was calibrated on a DIFFERENT INSTRUMENT than the
        // one that ships.** 0.20 came from `ffmpeg silencedetect` at sample level
        // over 83 segments, where iphone11 read 0.00 s of gap in 14 consecutive
        // files; the stored metric is 0.1 s RMS buckets and reads 56% of that
        // source's speaking minutes above the line. At 56% base rate, 14 clean in
        // a row is about 1 in 100,000 — so this is not sampling, the two measures
        // disagree, and which is right is unresolved. Agreement was checked on
        // THREE files and that was not enough to carry a threshold.
        //
        // ⚠ **The replacement rule is `processed.rs`, and it is also OFF.** It
        // reads a source's median speech-to-floor gap, which separates a gating
        // device from a healthy one by tens of dB and clears a repair within
        // ten segments; the numbers are in #1526. Every contributor RECORDS its
        // `gap_db`, so what the rule would have decided is re-derivable from
        // provenance without it ever having acted.
        //
        // A per-source median of `quiet_run_s` was the other candidate and lost
        // (#1526): it needs fifty reference rows, so it carried a repaired
        // device's old signature for weeks. Do not rebuild it.
        //
        // Two reasons it stays off, and the second is the stronger:
        //
        //   - #1461's referee has not run, so there is still no evidence that
        //     dropping a source improves a transcript rather than removing one.
        //   - It can only pay while some device is actually gating. Read the
        //     recorded `gap_db` signatures to find out rather than assuming
        //     either way — with none gating, every firing is a false positive.
        //     gating again.
        let ungated: Vec<&Contributor> = audible.clone();
        if ungated.is_empty() {
            // ⚠ Every microphone that heard this minute was gating. There is no
            // honest room block to build, and picking the least-bad would put a
            // transcript into the archive that nobody can tell apart from a good
            // one. The contributors are recorded, so the verdict is re-derivable
            // if the threshold ever moves.
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
            // Which RULE chose is part of the verdict: a later census must be
            // able to separate calibrated blocks from fallback ones without
            // re-deriving the reference that existed at the time.
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

/// Was this block already judged? (Read side for tests and, later, the API.)
pub fn verdict_of(conn: &Connection, block_start_utc: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT verdict FROM room_blocks WHERE start_utc = ?1",
        [block_start_utc],
        |r| r.get(0),
    )
    .optional()
}

/// Fill `coverage` for blocks judged before it was measured.
///
/// Read-only about the verdict: it records how much of each block its winner
/// actually recorded and changes nothing else. Blocks already found sparse keep
/// their verdict; what this produces is the distribution needed to decide what
/// to do about the ones that were built anyway (#1661).
///
/// # Errors
/// If the database refuses.
pub fn backfill_coverage(root: &Path, limit: usize) -> rusqlite::Result<usize> {
    let conn = store::open(root)?;
    ensure_schema(&conn)?;
    let pending: Vec<(String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT start_utc, winner FROM room_blocks
              WHERE coverage IS NULL AND winner IS NOT NULL
              ORDER BY start_utc DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([u32::try_from(limit).unwrap_or(u32::MAX)], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        rows.collect::<Result<_, _>>()?
    };
    let mut written = 0;
    for (start_utc, winner) in pending {
        let Ok(block) = DateTime::parse_from_rfc3339(&start_utc) else {
            continue;
        };
        let window = decode::window_covered(
            &root.join("ingest"),
            &winner,
            block.with_timezone(&Utc),
            BLOCK_S as usize,
            RATE,
        );
        conn.execute(
            "UPDATE room_blocks SET coverage = ?1 WHERE start_utc = ?2",
            rusqlite::params![f64::from(window.coverage), start_utc],
        )?;
        written += 1;
    }
    Ok(written)
}
