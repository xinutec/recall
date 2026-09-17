//! Record each closed segment in the Mac's `audio_segments`, so the loss alarm
//! has coverage to compare capture events against.
//!
//! ⚠ **THIS EXISTS BECAUSE THE ALARM WAS REPORTING AUDIO WE HAVE AS AUDIO WE
//! LOST** (#1650). `capture/speech-loss` reads coverage from this table and
//! explanations from `capture_events`; audiod writes the events, but the only
//! writer of the segments was Python's `recall index`, which no agent has run
//! since the Rust runner took over. The table froze on 2026-09-13 while events
//! kept arriving, so every minute recorded afterwards read as an unexplained
//! gap — 2.9 minutes of it by the time this was found, growing with every
//! recording.
//!
//! ⚠ A frozen coverage table looks EXACTLY like a silent recorder, which is the
//! alarm's own subject. That is why nothing caught it: the check was working
//! perfectly on an input that had stopped being true.
//!
//! ⚠ **The end time is DECODED, not assumed.** A segment file's header carries
//! no duration (measured — `audiocore::decode::stream_shape` says so), and the
//! two cheap guesses are both wrong in the dangerous direction: the segmenter's
//! nominal length over-claims a clip cut short by a pause, and "ends where the
//! next one starts" claims coverage straight across a pause. Both would hide
//! exactly the loss this table exists to expose.

use audiocore::names::{parse_segment_start, segment_glob};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

/// The rate the duration is measured at. Any rate answers the question — the
/// byte count divides by whatever was asked for — and 16 kHz is what the VAD
/// already decodes to, so a clip in the page cache stays there.
const MEASURE_RATE: u32 = 16_000;

/// How recently a file may have been written and still be skipped.
///
/// ⚠ Same three minutes as the speech scanner and the delivery check, for the
/// same reason: the newest file in a source directory is not late, it is
/// UNFINISHED. Registering it would stamp a permanent end time onto a clip
/// ffmpeg is still appending to.
const OPEN_GRACE_MINUTES: i64 = 3;

fn open(root: &Path) -> rusqlite::Result<Connection> {
    // READ_WRITE without CREATE, as everywhere else here: the migrations own
    // the file, and a missing database is a deployment fault to report.
    let conn = Connection::open_with_flags(
        root.join("recall.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
    )?;
    conn.busy_timeout(std::time::Duration::from_secs(30))?;
    Ok(conn)
}

/// The device sources this pass covers, from the meaning plane's own table.
///
/// ⚠ Devices only. An uploaded meeting is a source with no recorder that could
/// stop or lose speech, and its rows are written where it arrives.
fn device_sources(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT id FROM sources WHERE kind != 'upload'")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    rows.collect()
}

/// One closed clip with no row yet.
#[derive(Debug, PartialEq, Eq)]
pub struct Unregistered {
    pub source: String,
    pub path: PathBuf,
    pub start_utc: String,
}

/// Closed segments with no `audio_segments` row, oldest first.
///
/// ⚠ **OLDEST first, unlike the speech scanner.** That pass is answering "what
/// was said recently"; this one is filling a hole in a continuous record, and a
/// coverage table with gaps in the middle makes the loss check report them.
///
/// # Errors
/// If the meaning plane refuses.
pub fn unregistered(
    conn: &Connection,
    root: &Path,
    now: DateTime<Utc>,
    limit: usize,
) -> rusqlite::Result<Vec<Unregistered>> {
    let cutoff = now - Duration::minutes(OPEN_GRACE_MINUTES);
    let mut out = Vec::new();
    for source in device_sources(conn)? {
        let known: std::collections::HashSet<String> = {
            let mut stmt =
                conn.prepare("SELECT start_utc FROM audio_segments WHERE source_id = ?1")?;
            let rows = stmt.query_map([&source], |r| r.get::<_, String>(0))?;
            rows.collect::<Result<_, _>>()?
        };
        for path in segment_glob(&root.join(&source), &source) {
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(start) = parse_segment_start(name) else {
                continue;
            };
            if start >= cutoff {
                continue; // still being written
            }
            // ⚠ **SECONDS, and this cost 241 duplicate rows to learn.** The
            // column holds what Python's `datetime.isoformat()` wrote, and that
            // omits microseconds when they are zero — a segment start always is,
            // since the name it is parsed from is `YYYYmmddTHHMMSS`. Writing
            // `.000000+00:00` matched nothing in `known`, so every clip in the
            // archive looked unregistered and `INSERT OR IGNORE` let each one in
            // beside its twin: the UNIQUE key is the TEXT, and two spellings of
            // one instant are two keys.
            let start_utc = start.to_rfc3339_opts(SecondsFormat::Secs, false);
            if known.contains(&start_utc) {
                continue;
            }
            out.push(Unregistered {
                source: source.clone(),
                path,
                start_utc,
            });
        }
    }
    out.sort_by(|a, b| a.start_utc.cmp(&b.start_utc));
    out.truncate(limit);
    Ok(out)
}

/// What one pass did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pass {
    pub registered: usize,
    pub unreadable: usize,
}

/// ⚠ **A pass where EVERYTHING failed is the instrument, not the audio** — the
/// speech scanner's lesson, and it applies harder here because this pass writes
/// a row that is never revisited. Every file failing means ffmpeg is missing or
/// the volume is not mounted, and recording that as "these clips are unreadable"
/// would retire real audio from the coverage record for good.
fn credible(pass: &Pass) -> bool {
    pass.registered > 0 || pass.unreadable <= 1
}

/// Register up to `limit` closed segments.
///
/// # Errors
/// If the meaning plane refuses, or if nothing decoded at all.
pub fn run(root: &Path, limit: usize) -> Result<Pass, Box<dyn std::error::Error>> {
    // A store-and-forward recorder has no meaning plane to register into, and
    // that is its shape rather than a fault (`store::has_meaning_plane`).
    if !crate::store::has_meaning_plane(root) {
        return Ok(Pass::default());
    }
    let conn = open(root)?;
    let work = unregistered(&conn, root, Utc::now(), limit)?;
    if work.is_empty() {
        return Ok(Pass::default());
    }
    // Measured into memory first; nothing is written until the pass looks like a
    // reading of the audio rather than of the machine.
    let mut rows = Vec::with_capacity(work.len());
    let mut pass = Pass::default();
    for item in work {
        let shape = audiocore::decode::stream_shape(&item.path);
        let pcm = audiocore::decode::decode_s16(&item.path, MEASURE_RATE);
        if let (Some((rate, channels)), Some(pcm)) = (shape, pcm) {
            let seconds = pcm.len() as f64 / 2.0 / f64::from(MEASURE_RATE);
            pass.registered += 1;
            rows.push((item, rate, channels, seconds));
        } else {
            tracing::warn!(path = %item.path.display(), "did not probe or decode");
            pass.unreadable += 1;
        }
    }
    if !credible(&pass) {
        return Err(format!(
            "every one of {} segments failed to read — refusing to record that. \
             The likeliest cause is this process, not the audio: is ffmpeg on PATH?",
            pass.unreadable
        )
        .into());
    }
    for (item, rate, channels, seconds) in rows {
        let start = DateTime::parse_from_rfc3339(&item.start_utc)?.with_timezone(&Utc);
        let end = start + Duration::microseconds((seconds * 1e6).round() as i64);
        conn.execute(
            "INSERT OR IGNORE INTO audio_segments
                 (source_id, path, start_utc, end_utc, sample_rate, channels)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                item.source,
                item.path.to_string_lossy(),
                item.start_utc,
                // Microseconds HERE, unlike the start: a clip's length is not a
                // whole number of seconds and `isoformat()` would have written them.
                end.to_rfc3339_opts(SecondsFormat::Micros, false),
                rate,
                channels
            ],
        )?;
    }
    Ok(pass)
}
