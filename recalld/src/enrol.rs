//! Voiceprint enrolment (stage E4): turning a named turn into a reference vector.
//!
//! ⚠ **This is the last thing keeping the Mac's Python alive.** `sync` and
//! `sync_push` exist to replay the fleet's namings onto the Mac so the Mac's
//! `identify.backfill_voiceprints` can enrol them; with enrolment here, the
//! namings never have to leave the machine that already holds them (#1538).
//!
//! ⚠ **Enrolling more is not the same as identifying better.** Measured
//! 2026-09-17 (#1648): going from 750 prints to 972 moved attribution +0.19
//! points, and 187 prints score within 1.3 of 972. The corpus has saturated, so
//! this pass exists to remove a Python loop — not to raise the number. Anyone
//! proposing work here on quality grounds should re-run that ablation first.
//!
//! The work-list mirrors `recall.store.turns_needing_voiceprint` exactly, and
//! that is deliberate: the two must select the same turns while both exist, or a
//! turn enrolled by one is re-enrolled by the other under a second print.

use crate::queue::ENROLL_SPEAKER;
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// A turn short enough that its clip enrols a useless print.
///
/// ⚠ Spelled to match `recall.store._MIN_VOICEPRINT_SECONDS`. The loudness half
/// of the Python gate is NOT carried across: `set_loudness` has had no production
/// caller since the API moved, so every row reads NULL and the gate keeps all of
/// them. Porting an inert filter would have made it look enforced.
const MIN_SECONDS: f64 = 1.0;

/// One turn to embed, as the runner is told about it.
///
/// ⚠ **No name on the wire.** The runner does not need to know whose voice it
/// is, and the fleet must read the label at WRITE time anyway — a turn
/// re-assigned between lease and result would otherwise enrol the old name.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Span {
    pub segment_id: i64,
    /// Seconds from the start of the clip, not from the epoch.
    pub start_s: f64,
    pub end_s: f64,
}

/// A clip's identity without its container — the join key between the planes.
///
/// ⚠ The same recording exists under two extensions (the ingest copy is often
/// `.wav` where the meaning plane's path is the `.opus` mirror), so whole
/// filenames compare unequal for the same audio. Same reasoning as
/// `queue::derive_segment_jobs`, and the same trap.
fn stem(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.')
        .map_or(name, |(base, _)| base)
        .to_owned()
}

/// Every turn awaiting enrolment, grouped by the stem of the clip it sits in.
///
/// # Errors
/// If the meaning plane refuses.
pub fn pending(meaning: &Connection) -> rusqlite::Result<Vec<(String, Span)>> {
    let mut stmt = meaning.prepare(
        "SELECT a.path, t.id, t.start_utc, t.end_utc, a.start_utc
         FROM transcript_segments t
         JOIN audio_segments a ON a.id = t.audio_segment_id
         WHERE t.speaker_label IS NOT NULL
           AND t.speaker_label NOT LIKE 'SPEAKER%'
           AND t.superseded_by IS NULL
           AND t.hidden_reason IS NULL
           AND t.id NOT IN (
             SELECT source_segment_id FROM speaker_embeddings
             WHERE source_segment_id IS NOT NULL)
         ORDER BY t.start_utc DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            stem(&r.get::<_, String>(0)?),
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (clip, segment_id, start, end, clip_start) = row?;
        // ⚠ Parsed and subtracted here rather than by SQLite's `julianday`.
        // That function counts DAYS in a double, so a span it returns is a few
        // tens of microseconds off the instant the row actually holds — and the
        // Python this must agree with subtracts real datetimes. Two enrolment
        // passes disagreeing in the sixth decimal is not a bug today, but it is
        // the kind that is only ever found by someone diffing two archives.
        let (Ok(start), Ok(end), Ok(clip_start)) = (
            DateTime::parse_from_rfc3339(&start),
            DateTime::parse_from_rfc3339(&end),
            DateTime::parse_from_rfc3339(&clip_start),
        ) else {
            continue; // a row whose instant will not parse names no span
        };
        let seconds = |a: DateTime<chrono::FixedOffset>| {
            (a - clip_start).num_microseconds().map_or_else(
                || (a - clip_start).num_seconds() as f64,
                |us| us as f64 / 1e6,
            )
        };
        if (end - start).num_milliseconds() < (MIN_SECONDS * 1000.0) as i64 {
            continue; // a sliver enrols a useless print
        }
        out.push((
            clip,
            Span {
                segment_id,
                // ⚠ Clamped at zero, never negative. A turn whose start rounds
                // a hair before its clip's would ask ffmpeg to seek backwards,
                // and ffmpeg answers that with the whole clip rather than an
                // error — enrolling a minute of the room as one person's voice.
                start_s: seconds(start).max(0.0),
                end_s: seconds(end),
            },
        ));
    }
    Ok(out)
}

/// The turns to embed from one leased clip, by its INGEST filename.
///
/// # Errors
/// If the meaning plane refuses.
pub fn spans_for(meaning: &Connection, filename: &str) -> rusqlite::Result<Vec<Span>> {
    let want = stem(filename);
    Ok(pending(meaning)?
        .into_iter()
        .filter(|(s, _)| *s == want)
        .map(|(_, span)| span)
        .collect())
}

/// Derive one enrolment job per CLIP holding turns that still need a voiceprint.
///
/// ⚠ **Per clip, not per turn**, because `jobs` is keyed `UNIQUE (kind,
/// filename)` and a clip routinely holds several named turns. The spans travel
/// with the lease; the job names only the audio to fetch.
///
/// ⚠ Derived from the INGEST side, so a turn whose clip was never delivered gets
/// no job — the runner could not fetch it, and a job it cannot do would burn its
/// attempts against audio that is not there.
///
/// # Errors
/// If either database refuses.
pub fn derive_jobs(
    ingest: &Connection,
    meaning: &Connection,
    now: DateTime<Utc>,
    limit: usize,
) -> rusqlite::Result<usize> {
    let wanted: std::collections::HashSet<String> =
        pending(meaning)?.into_iter().map(|(s, _)| s).collect();
    if wanted.is_empty() {
        return Ok(0);
    }
    let candidates: Vec<String> = {
        let mut stmt = ingest.prepare(
            "SELECT s.filename FROM segments s
             WHERE NOT EXISTS (SELECT 1 FROM jobs j
                               WHERE j.kind = ?1 AND j.filename = s.filename)
             ORDER BY s.start_utc DESC",
        )?;
        let rows = stmt.query_map([ENROLL_SPEAKER], |r| r.get::<_, String>(0))?;
        rows.collect::<Result<_, _>>()?
    };
    let mut inserted = 0;
    for filename in candidates {
        if inserted >= limit {
            break;
        }
        if !wanted.contains(&stem(&filename)) {
            continue;
        }
        inserted += ingest.execute(
            "INSERT OR IGNORE INTO jobs (kind, filename, created_utc) VALUES (?1, ?2, ?3)",
            (
                ENROLL_SPEAKER,
                &filename,
                now.to_rfc3339_opts(SecondsFormat::Secs, true),
            ),
        )?;
    }
    Ok(inserted)
}
