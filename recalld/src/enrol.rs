//! Voiceprint enrolment: turning a named turn into a reference vector, on the
//! fleet, where the namings are. Enrolling more does not identify better: the
//! print corpus has saturated (#1648), so this keeps up with new labels rather
//! than raising a number.

use crate::ledger::{Outcome, record};
use audiocore::instant::Stamp;
use audiocore::job::Kind;
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

crate::statements! {
    PENDING: Meaning =
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
         ORDER BY t.start_utc DESC";
    UNDERIVED: Ingest =
        "SELECT s.filename FROM segments s
             WHERE NOT EXISTS (SELECT 1 FROM jobs j
                               WHERE j.kind = ?1 AND j.filename = s.filename)
             ORDER BY s.start_utc DESC";
    DERIVE_JOB: Ingest =
        "INSERT OR IGNORE INTO jobs (kind, filename, created_utc) VALUES (?1, ?2, ?3)";
    STILL_WANTED: Meaning =
        "SELECT t.speaker_label FROM transcript_segments t
             WHERE t.id = ?1
               AND t.speaker_label IS NOT NULL
               AND t.speaker_label NOT LIKE 'SPEAKER%'
               AND t.superseded_by IS NULL
               AND t.hidden_reason IS NULL
               AND t.id NOT IN (
                 SELECT source_segment_id FROM speaker_embeddings
                 WHERE source_segment_id IS NOT NULL)";
    ADD_SPEAKER: Meaning =
        "INSERT OR IGNORE INTO speakers (name) VALUES (?1)";
    SPEAKER_ID: Meaning =
        "SELECT id FROM speakers WHERE name = ?1";
    ADD_EMBEDDING: Meaning =
        "INSERT INTO speaker_embeddings
             (speaker_id, vector, created_utc, source_correction_id, source_segment_id)
         VALUES (?1, ?2, ?3, NULL, ?4)";
    FINISHED: Ingest =
        "SELECT j.filename, j.result FROM jobs j
             WHERE j.kind = ?1 AND j.done_utc IS NOT NULL AND j.result IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM pass_ledger l
                               WHERE l.kind = ?1 AND l.filename = j.filename)
             ORDER BY j.filename ASC";
}

/// A turn shorter than this enrols a useless print.
const MIN_SECONDS: f64 = 1.0;

/// One turn to embed, as the runner is told about it. No name on the wire: the
/// fleet reads the label at write time, so a turn re-assigned between lease and
/// result enrols under the name it has then.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Span {
    pub segment_id: i64,
    /// Seconds from the start of the clip, not from the epoch.
    pub start_s: f64,
    pub end_s: f64,
}

/// A clip's identity without its container, the join key between the planes:
/// the same recording can exist under two extensions.
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
    let mut stmt = PENDING.prepare(meaning)?;
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
        // Subtracted here rather than by SQLite's `julianday`, which counts
        // days in a double and is off by tens of microseconds.
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
                // Clamped at zero: a negative seek makes ffmpeg return the
                // whole clip, enrolling a minute of the room as one voice.
                start_s: seconds(start).max(0.0),
                end_s: seconds(end),
            },
        ));
    }
    Ok(out)
}

/// The turns to embed from one leased clip, by its ingest filename.
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

/// Fill a leased job's spans, if it is a kind that has any. At lease time, not
/// at derivation: the work-list lives in the meaning plane and changes whenever
/// somebody renames a voice. The meaning plane is opened only for a kind that
/// needs it, so a transcription runner is not stopped by a database it never
/// reads.
///
/// # Errors
/// If the meaning plane refuses.
pub fn attach_spans(root: &std::path::Path, job: &mut crate::queue::Job) -> rusqlite::Result<()> {
    if job.kind != Kind::EnrollSpeaker {
        return Ok(());
    }
    job.spans = spans_for(&crate::reads::open(root)?, &job.filename)?;
    Ok(())
}

/// Derive one enrolment job per clip holding turns that still need a
/// voiceprint. Per clip, because `jobs` is keyed on (kind, filename); the spans
/// travel with the lease. Derived from the ingest side, so a turn whose clip
/// was never delivered gets no job the runner could not fetch.
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
        let mut stmt = UNDERIVED.prepare(ingest)?;
        let rows = stmt.query_map([Kind::EnrollSpeaker], |r| r.get::<_, String>(0))?;
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
        inserted += DERIVE_JOB.execute(
            ingest,
            (
                Kind::EnrollSpeaker,
                &filename,
                now.to_rfc3339_opts(SecondsFormat::Secs, true),
            ),
        )?;
    }
    Ok(inserted)
}

// --- writing what the runner embedded ----------------------------------------

/// One embedded span as the runner sends it back.
pub use audiocore::shim::voices::Print;
use audiocore::shim::{Stored, voices::Prints};

/// What one pass did, for the log line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Enrolled {
    pub clips: usize,
    pub prints: usize,
    /// Spans whose turn no longer qualifies — re-named, hidden, or enrolled by
    /// something else while the runner was working.
    pub stale: usize,
}

/// Whose voice a segment is now, or `None` if it should no longer be enrolled.
/// Re-read at write time: a person can re-assign a turn while the model runs.
fn still_wanted(meaning: &Connection, segment_id: i64) -> rusqlite::Result<Option<String>> {
    STILL_WANTED
        .query_row(meaning, [segment_id], |r| r.get::<_, String>(0))
        .map(Some)
        .or_else(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })
}

/// Enrol one span under `person`, creating the speaker if this is their first.
///
/// # Errors
/// If the meaning plane refuses.
fn enrol_one(
    meaning: &Connection,
    person: &str,
    print: &Print,
    now: &Stamp,
) -> rusqlite::Result<()> {
    ADD_SPEAKER.execute(meaning, [person])?;
    let speaker_id: i64 = SPEAKER_ID.query_row(meaning, [person], |r| r.get(0))?;
    ADD_EMBEDDING.execute(
        meaning,
        rusqlite::params![
            speaker_id,
            serde_json::to_string(&print.vector).unwrap_or_else(|_| "[]".to_owned()),
            now,
            print.segment_id
        ],
    )?;
    Ok(())
}

/// Turn finished `enroll-speaker` results into reference voiceprints. Every
/// clip examined is ledgered, including one that enrols nothing: the candidate
/// query is "not in the ledger".
///
/// # Errors
/// If either database refuses.
pub fn write_pass(
    meaning: &Connection,
    ingest: &Connection,
    now: &Stamp,
    limit: usize,
) -> rusqlite::Result<Enrolled> {
    let candidates: Vec<(String, String)> = {
        let mut stmt = FINISHED.prepare(ingest)?;
        let rows = stmt.query_map([Kind::EnrollSpeaker], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<Result<_, _>>()?
    };

    let mut pass = Enrolled::default();
    for (filename, result) in candidates {
        if pass.clips >= limit {
            break;
        }
        pass.clips += 1;
        let Ok(reply) = Stored::<Prints>::parse(&result) else {
            record(
                ingest,
                Kind::EnrollSpeaker,
                &filename,
                Outcome::Unreadable,
                None,
                now,
            )?;
            continue;
        };
        let Some(body) = reply.result.filter(|_| reply.ok) else {
            record(
                ingest,
                Kind::EnrollSpeaker,
                &filename,
                Outcome::Refused,
                None,
                now,
            )?;
            continue;
        };
        let mut wrote = 0;
        for print in &body.prints {
            // An empty vector is not a voiceprint: at cosine 0 against everyone
            // it becomes somebody's best match on quiet audio.
            if print.vector.is_empty() {
                pass.stale += 1;
                continue;
            }
            match still_wanted(meaning, print.segment_id)? {
                Some(person) => {
                    enrol_one(meaning, &person, print, now)?;
                    wrote += 1;
                }
                None => pass.stale += 1,
            }
        }
        pass.prints += wrote;
        let outcome = if wrote > 0 {
            Outcome::Enrolled
        } else {
            Outcome::NothingEnrolled
        };
        record(ingest, Kind::EnrollSpeaker, &filename, outcome, None, now)?;
    }
    Ok(pass)
}
