//! How the instant feed is doing, measured where its output actually lands.
//!
//! ⚠ **`recall-live` is a MAC agent that keeps no store**, so the only copy of
//! what it produced is here. The Mac's own archive holds live turns from before
//! that moved and nothing since, which is why the doctor's two live checks were
//! reading a database the tier had stopped writing to (#1671).
//!
//! ⚠ **This measures and does not judge.** Every threshold, every skip rule and
//! every verdict stays in the doctor, so a person reading fleetwatch sees one
//! grader rather than two that can disagree about what "behind" means. The
//! caller names its own windows for the same reason.

use crate::store;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;

/// The transcripts written by the instant tier rather than an archive pass.
const LIVE_MODEL: &str = "live";

/// The numbers the doctor's live checks are computed from.
///
/// ⚠ **`lagSamples` travels beside the median** because a median over a handful
/// of turns reports noise as a regression. How many is enough is the doctor's
/// rule; sending only the median would take that decision away from it.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LiveHealth {
    /// Median seconds between a live turn's end and its arrival. `None` when
    /// no turn in the window carried both stamps.
    pub lag_median_s: Option<f64>,
    /// How many turns that median is over.
    pub lag_samples: usize,
    /// The newest live turn's start, however old.
    pub newest_turn_utc: Option<String>,
    /// Seconds of audio the device recorders delivered in the window.
    pub delivered_s: f64,
    /// Seconds of that audio the speech scanner has measured.
    pub scanned_s: f64,
    /// Seconds of speech found within the scanned part.
    pub speech_s: f64,
}

/// Everything the doctor's live checks need, out of both planes.
///
/// ⚠ **The speech evidence spans two databases and there is no join.**
/// `audio_segments` (meaning plane) knows when a clip started and ended;
/// `segment_speech` (ingest plane) knows how much of it was speech. They are
/// matched in memory on the clip's filename rather than by `ATTACH`, so neither
/// plane's connection outlives the other's read.
///
/// ⚠ **The window bounds are RE-SPELLED, not passed through.** Every stored
/// timestamp is compared as TEXT, so `…T11:40:00Z` and `…T11:40:00+00:00` are
/// the same moment and two different values to every query below — and `Z`
/// sorts AFTER `+`, so a caller using it would silently lose the first row of
/// its own window. [`audiocore::instant`] holds the one spelling.
///
/// ⚠ It must NOT reach for `audio_segments.speech_s`. That column is the Mac's,
/// filled by `audiod speech`, and the fleet's copy stopped receiving it in July
/// — a reader that trusted it would see an unmeasured window forever, which the
/// doctor correctly refuses to call quiet, so every silent night would fail.
pub fn live_health(
    root: &Path,
    lag_since: DateTime<Utc>,
    window_since: DateTime<Utc>,
    window_until: DateTime<Utc>,
) -> rusqlite::Result<LiveHealth> {
    let (lag_since, window_since, window_until) = (
        audiocore::instant::python_isoformat_utc(lag_since),
        audiocore::instant::python_isoformat_utc(window_since),
        audiocore::instant::python_isoformat_utc(window_until),
    );
    let meaning = crate::reads::open(root)?;
    let lags = live_lags(&meaning, &lag_since)?;
    let newest_turn_utc = meaning.query_row(
        "SELECT max(start_utc) FROM transcript_segments WHERE asr_model = ?1",
        [LIVE_MODEL],
        |row| row.get(0),
    )?;
    let clips = window_clips(&meaning, &window_since, &window_until)?;
    drop(meaning);

    let names: Vec<&str> = clips.iter().map(|(name, _)| name.as_str()).collect();
    let speech = speech_seconds(&store::open(root)?, &names)?;
    let mut out = LiveHealth {
        lag_median_s: median(lags.clone()),
        lag_samples: lags.len(),
        newest_turn_utc,
        delivered_s: 0.0,
        scanned_s: 0.0,
        speech_s: 0.0,
    };
    for (filename, seconds) in clips {
        out.delivered_s += seconds;
        if let Some(measured) = speech.get(&filename) {
            out.scanned_s += seconds;
            out.speech_s += measured;
        }
    }
    Ok(out)
}

/// Seconds between each recent live turn's end and the moment it was stored.
fn live_lags(conn: &Connection, since: &str) -> rusqlite::Result<Vec<f64>> {
    let mut stmt = conn.prepare(
        "SELECT created_utc, end_utc FROM transcript_segments \
         WHERE asr_model = ?1 AND created_utc IS NOT NULL AND end_utc >= ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![LIVE_MODEL, since], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut lags = Vec::new();
    for row in rows {
        let (created, end) = row?;
        if let (Some(created), Some(end)) = (
            audiocore::instant::parse(&created),
            audiocore::instant::parse(&end),
        ) {
            lags.push((created - end).num_milliseconds() as f64 / 1000.0);
        }
    }
    Ok(lags)
}

/// ⚠ THE MEDIAN, never the mean. One clip that waited behind a restart moves a
/// mean by minutes and says nothing about the tier.
fn median(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    Some(values[values.len() / 2])
}

/// Each device clip that started inside the window, and how long it ran.
///
/// ⚠ **Which sources count is [`crate::sources::SourceKind::is_device`]'s to
/// say, not this query's.** Spelling the rule again in SQL would be a second
/// answer to "is there a recorder behind this" — an imported meeting and the
/// derived room stream are both sources with no microphone, and counting
/// either as delivered audio says the room was busy when a file was uploaded.
fn window_clips(
    conn: &Connection,
    since: &str,
    until: &str,
) -> rusqlite::Result<Vec<(String, f64)>> {
    let devices: Vec<String> = crate::sources::source_rows(conn)?
        .into_iter()
        .filter(|row| row.kind.is_device())
        .map(|row| row.id)
        .collect();
    let mut stmt = conn.prepare(
        "SELECT path, start_utc, end_utc FROM audio_segments \
         WHERE source_id = ?1 AND path IS NOT NULL \
           AND start_utc >= ?2 AND start_utc < ?3",
    )?;
    let mut out = Vec::new();
    for source in devices {
        let rows = stmt.query_map(rusqlite::params![source, since, until], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (path, start, end) = row?;
            let (Some(start), Some(end)) = (
                audiocore::instant::parse(&start),
                audiocore::instant::parse(&end),
            ) else {
                continue;
            };
            let seconds = (end - start).num_milliseconds() as f64 / 1000.0;
            if seconds > 0.0 {
                out.push((basename(&path), seconds));
            }
        }
    }
    Ok(out)
}

/// The clip's filename, which is what the ingest plane keys on.
fn basename(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_owned()
}

/// How much speech each named clip carries, for the ones that have been
/// measured.
///
/// ⚠ **A clip that would not decode is UNMEASURED, not silent.**
/// [`audiocore::vad::UNKNOWN_SECONDS`] is negative precisely so it cannot be
/// mistaken for a duration, and adding one to a window would subtract a second
/// of speech from it — pushing a genuinely busy window under the floor that
/// decides whether the house was talking. Leaving it out counts the clip as
/// delivered but unscanned, which is what it is.
fn speech_seconds(conn: &Connection, names: &[&str]) -> rusqlite::Result<HashMap<String, f64>> {
    let mut out = HashMap::new();
    if names.is_empty() {
        return Ok(out);
    }
    let mut stmt = conn.prepare(
        "SELECT speech_seconds FROM segment_speech WHERE filename = ?1 AND speech_seconds >= 0",
    )?;
    for name in names {
        if let Some(seconds) = stmt
            .query_row([name], |row| row.get::<_, f64>(0))
            .optional()?
        {
            out.insert((*name).to_owned(), seconds);
        }
    }
    Ok(out)
}
