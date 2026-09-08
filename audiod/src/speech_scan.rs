//! Measure how much of each archived segment is SPEECH, on the Mac.
//!
//! ⚠ **This is a deletion guard, which is why it runs here and not on Isis.**
//! `speech_s = 0.0` is what lets the quiet review propose removing a segment;
//! the Mac is the master archive and removing audio from it is a Mac-local act
//! (docs/architecture.md, "Deletion authority"). A guard that had to fetch its
//! evidence over the network would either block cleanup whenever the fleet is
//! unreachable, or — far worse — proceed without it.
//!
//! And the fleet's copy could not answer for everything even when reachable:
//! measured 2026-09-08, 730 of 14,777 Mac segments had never been delivered.
//! Those are the least replicated audio in the house, so a blind spot there is
//! exactly where an accident happens.
//!
//! ⚠ It uses `audiocore::vad` — the SAME detector the fleet runs, not an
//! equivalent one. Two detectors disagreeing about what counts as speech is not
//! a discrepancy to reconcile later: one of the two answers is a licence to
//! delete audio somebody was talking in.
//!
//! Newest first, mirroring `recalld::speech`: the readers that matter ask about
//! recent audio, and oldest-first would make a 13k-segment backlog block them
//! for hours behind audio from May.

use audiocore::vad::{Detector, UNKNOWN_SECONDS};
use chrono::{DateTime, Duration, Utc};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

/// One segment waiting to be listened to.
struct Pending {
    id: i64,
    path: PathBuf,
}

fn open(root: &Path) -> rusqlite::Result<Connection> {
    // READ_WRITE without CREATE, as everywhere else here: the Python migrations
    // own this file, and a missing database is a deployment fault to report
    // rather than something to half-create.
    let conn = Connection::open_with_flags(
        root.join("recall.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
    )?;
    conn.busy_timeout(std::time::Duration::from_secs(30))?;
    Ok(conn)
}

/// How recently a segment may have ended and still be skipped.
///
/// ⚠ Bought by breaking it: the first run of this scanner took the five NEWEST
/// segments, which ffmpeg was still writing, failed to decode every one, and
/// recorded them as unlistened — a permanent wrong answer, because a row with a
/// value is never revisited. The files were fine; they were 185 KB of ordinary
/// speech a minute later. Newest-first is right for usefulness and wrong at the
/// boundary, and this is the boundary.
///
/// Three minutes matches the delivery check's open grace, which exists for the
/// same reason: the newest file in a source directory is not late, it is
/// unfinished.
const OPEN_GRACE_MINUTES: i64 = 3;

fn pending(conn: &Connection, limit: usize, now: DateTime<Utc>) -> rusqlite::Result<Vec<Pending>> {
    let cutoff = (now - Duration::minutes(OPEN_GRACE_MINUTES))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
    let mut stmt = conn.prepare(
        "SELECT id, path FROM audio_segments
          WHERE speech_s IS NULL AND path IS NOT NULL AND end_utc < ?2
          ORDER BY start_utc DESC
          LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![limit, cutoff], |row| {
            Ok(Pending {
                id: row.get(0)?,
                path: PathBuf::from(row.get::<_, String>(1)?),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn record(conn: &Connection, id: i64, seconds: f64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE audio_segments SET speech_s = ?1 WHERE id = ?2",
        rusqlite::params![seconds, id],
    )?;
    Ok(())
}

/// What one pass did, so the caller can say it without recounting.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pass {
    pub measured: usize,
    pub unreadable: usize,
}

/// Measure up to `limit` unmeasured segments, newest first.
///
/// ⚠ A segment that cannot be decoded is recorded as [`UNKNOWN_SECONDS`], never
/// skipped and never zero. Skipping would make the pass retry the same broken
/// file for ever; zero would tell the quiet review that nobody spoke in audio
/// nothing ever listened to.
/// ⚠ **A pass where EVERYTHING failed is the instrument, not the audio.**
///
/// Bought twice in ten minutes, both times damaging real rows. The scanner
/// stamped 5 segments unlistened because ffmpeg had not finished writing them,
/// and then 20 more because `ffmpeg` was not on PATH at all — `decode_s16`
/// shells out to it. Neither had anything to do with the recordings; both wrote
/// a verdict that is never revisited, onto audio nothing had listened to.
///
/// A broken file among good ones is believable. Every file broken is a missing
/// decoder, a wrong root, an unmounted volume — and recording that as "we could
/// not look at any of this" is worse than recording nothing, because it retires
/// the segments from ever being measured.
fn credible(pass: &Pass) -> bool {
    pass.measured > 0 || pass.unreadable <= 1
}

pub fn run(root: &Path, limit: usize) -> Result<Pass, Box<dyn std::error::Error>> {
    let conn = open(root)?;
    let work = pending(&conn, limit, Utc::now())?;
    if work.is_empty() {
        return Ok(Pass::default());
    }
    let mut detector = Detector::load()?;

    // Measure into memory FIRST. Nothing is written until the pass as a whole
    // looks like a reading of the audio rather than a reading of the machine.
    let mut results = Vec::with_capacity(work.len());
    let mut pass = Pass::default();
    for item in work {
        match detector.speech_seconds(&item.path) {
            Ok(seconds) => {
                pass.measured += 1;
                results.push((item.id, seconds));
            }
            Err(err) => {
                tracing::warn!(id = item.id, path = %item.path.display(), %err,
                    "did not decode");
                pass.unreadable += 1;
                results.push((item.id, UNKNOWN_SECONDS));
            }
        }
    }

    if !credible(&pass) {
        return Err(format!(
            "every one of {} segments failed to decode — refusing to record that. \
             The likeliest cause is this process, not the audio: is ffmpeg on PATH?",
            pass.unreadable
        )
        .into());
    }

    for (id, seconds) in results {
        record(&conn, id, seconds)?;
    }
    Ok(pass)
}

/// How many segments still have no measurement — for the log line, so a run
/// says how far it has to go rather than only what it just did.
pub fn remaining(root: &Path) -> rusqlite::Result<i64> {
    open(root)?.query_row(
        "SELECT count(*) FROM audio_segments WHERE speech_s IS NULL",
        [],
        |row| row.get(0),
    )
}
