//! How the instant feed is doing, measured where its output lands: the live
//! agent keeps no store of its own, so this is the only copy.
//!
//! This measures and does not judge. Thresholds, skip rules and verdicts stay
//! in the doctor, and the caller names its own windows, so there is one grader.

use crate::store;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;

use crate::turn_store::LIVE_MODEL;

/// The numbers the doctor's live checks are computed from.
///
/// `lagSamples` travels beside the median because a median over a handful of
/// turns is noise; how many is enough is the doctor's rule.
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
/// `audio_segments` (meaning plane) holds each clip's span; `segment_speech`
/// (ingest plane) holds how much of it was speech. They are matched in memory on
/// the clip's filename rather than by `ATTACH`.
///
/// ⚠ The window bounds are re-spelled with [`audiocore::instant`], because
/// timestamps are compared as text: `Z` sorts after `+00:00`, so a `Z` bound
/// would lose the first row of its window.
///
/// ⚠ Do not read `audio_segments.speech_s`: this server's copy of that column
/// is not filled, so every window would read as unmeasured.
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

    let names: Vec<&str> = clips.iter().map(|clip| clip.filename.as_str()).collect();
    let speech = speech_seconds(&store::open(root)?, &names)?;
    let mut out = LiveHealth {
        lag_median_s: median(lags.clone()),
        lag_samples: lags.len(),
        newest_turn_utc,
        delivered_s: 0.0,
        scanned_s: 0.0,
        speech_s: 0.0,
    };
    for clip in clips {
        out.delivered_s += clip.seconds;
        if let Some(measured) = speech.get(&clip.filename) {
            out.scanned_s += clip.seconds;
            out.speech_s += measured;
        }
    }
    Ok(out)
}

/// What one device source delivered and heard in a window.
///
/// Only measured clips count, on both sides of the ratio: an unmeasured clip is
/// unknown, not silent, and counting it would make a lagging scanner look like a
/// deaf microphone.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Heard {
    pub source: String,
    pub delivered_s: f64,
    pub speech_s: f64,
}

/// Per device source, what the doctor's deaf check compares: sources that
/// delivered no measured audio are left out, in source order.
///
/// # Errors
/// If either plane refuses the read.
pub fn heard(
    root: &Path,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
) -> rusqlite::Result<Vec<Heard>> {
    let clips = window_clips(
        &crate::reads::open(root)?,
        &audiocore::instant::python_isoformat_utc(since),
        &audiocore::instant::python_isoformat_utc(until),
    )?;
    let names: Vec<&str> = clips.iter().map(|clip| clip.filename.as_str()).collect();
    let speech = speech_seconds(&store::open(root)?, &names)?;
    let mut by_source: std::collections::BTreeMap<&str, Heard> = std::collections::BTreeMap::new();
    for clip in &clips {
        let Some(measured) = speech.get(&clip.filename) else {
            continue;
        };
        let entry = by_source.entry(&clip.source).or_insert_with(|| Heard {
            source: clip.source.clone(),
            delivered_s: 0.0,
            speech_s: 0.0,
        });
        entry.delivered_s += clip.seconds;
        entry.speech_s += measured;
    }
    Ok(by_source.into_values().collect())
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

/// The median, not the mean: one clip that waited behind a restart moves a mean
/// by minutes.
fn median(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    Some(values[values.len() / 2])
}

/// A device clip that started inside the window.
struct Clip {
    source: String,
    filename: String,
    seconds: f64,
}

/// Each device clip that started inside the window, and how long it ran.
///
/// Which sources count is decided by [`crate::sources::SourceKind::is_device`],
/// not restated in SQL: uploads and the derived room stream have no microphone.
fn window_clips(conn: &Connection, since: &str, until: &str) -> rusqlite::Result<Vec<Clip>> {
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
        let rows = stmt.query_map(rusqlite::params![&source, since, until], |row| {
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
                out.push(Clip {
                    source: source.clone(),
                    filename: basename(&path),
                    seconds,
                });
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
/// A clip that would not decode is unmeasured, not silent: its stored value is
/// the negative [`audiocore::vad::UNKNOWN_SECONDS`], which is filtered out so it
/// counts as delivered but unscanned instead of subtracting speech.
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
