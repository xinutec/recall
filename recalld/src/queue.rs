//! The work queue the Mac's runners poll.
//!
//! Jobs are derived from the blobs, never enqueued, so a missed enqueue cannot
//! strand audio. Leases are time-bounded: a dead runner's job is offered
//! again. Newest clip first: what is being said now outranks backfill.

use crate::same_speech::COPY_SECONDS;
use crate::store::{self, ROOM_SOURCE};
use audiocore::job::Kind;
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use std::path::Path;

crate::statements! {
    /// Ordered by capture time, not filename, which would drain one source
    /// before the next. Enrolment goes first: it follows what a person typed,
    /// usually on an old clip, and only a few are derived per pass.
    ///
    /// `?2` is the kinds the runner can do, as a JSON array; `?3` enrolment.
    LEASE: Ingest =
        "SELECT j.id, j.kind, j.filename, s.source FROM jobs j
         JOIN segments s ON s.filename = j.filename
         WHERE j.state IN ('queued', 'leased')
           AND (j.leased_until IS NULL OR j.leased_until < ?1)
           AND j.done_utc IS NULL
           AND j.kind IN (SELECT value FROM json_each(?2))
         ORDER BY (j.kind = ?3) DESC, s.start_utc DESC, j.filename DESC
         LIMIT 1";
    DERIVE_FOLLOW_ON: Ingest =
        "INSERT OR IGNORE INTO jobs (kind, filename, created_utc)
         SELECT ?1, j.filename, ?2 FROM jobs j
         WHERE j.kind = ?3 AND j.done_utc IS NOT NULL
           AND json_valid(j.result) AND json_extract(j.result, '$.ok') = 1
           AND NOT EXISTS (SELECT 1 FROM jobs d
                           WHERE d.kind = ?1 AND d.filename = j.filename)";
    TRANSCRIBED_PATHS: Meaning =
        "SELECT DISTINCT a.path FROM audio_segments a
             JOIN transcript_segments t ON t.audio_segment_id = a.id
             WHERE a.source_id != ?1";
    NON_ROOM_SOURCES: Meaning =
        "SELECT id FROM sources WHERE kind != ?1";
    /// Measured clips with no transcription job, nor one for another copy of
    /// the minute: a phone's minute arrives twice (the Mac's `.flac` of its
    /// stream, the phone's `.phone.flac`), stamped within
    /// [`COPY_SECONDS`](crate::same_speech::COPY_SECONDS). Newest first.
    UNQUEUED_SPEECH: Ingest =
        "SELECT s.filename, s.source, s.start_utc FROM segments s
             JOIN segment_speech p ON p.filename = s.filename
             WHERE s.source != ?1
               AND p.speech_seconds != 0.0
               AND NOT EXISTS (SELECT 1 FROM jobs j
                               WHERE j.kind = ?2 AND j.filename = s.filename)
               AND NOT EXISTS (SELECT 1 FROM segments o
                               JOIN jobs j ON j.filename = o.filename AND j.kind = ?2
                               WHERE o.source = s.source AND o.filename != s.filename
                                 AND o.start_utc BETWEEN
                                     strftime('%Y-%m-%dT%H:%M:%SZ', s.start_utc, printf('-%d seconds', ?3))
                                     AND strftime('%Y-%m-%dT%H:%M:%SZ', s.start_utc, printf('+%d seconds', ?3)))
             ORDER BY s.start_utc DESC, s.filename DESC";
    DERIVE_JOB: Ingest =
        "INSERT OR IGNORE INTO jobs (kind, filename, created_utc) VALUES (?1, ?2, ?3)";
    LEASE_JOB: Ingest =
        "UPDATE jobs SET state = 'leased', leased_until = ?1,
                             attempts = attempts + 1
             WHERE id = ?2";
    RETIRE_EXHAUSTED: Ingest =
        "UPDATE jobs SET state = 'done', done_utc = ?1, result = ?2
         WHERE done_utc IS NULL AND attempts >= ?3
           AND (leased_until IS NULL OR leased_until < ?1)";
    FINISH: Ingest =
        "UPDATE jobs SET state = 'done', done_utc = ?1, result = ?2
         WHERE id = ?3 AND done_utc IS NULL";
}

const LEASE_TTL_S: i64 = 10 * 60;
/// Leases a job may take before it is retired as failed; otherwise a clip that
/// kills the shim would hold the GPU for ever.
pub const MAX_ATTEMPTS: i64 = 3;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Job {
    pub id: i64,
    pub kind: Kind,
    /// Fetched via `/ingest/v1/blob/<source>/<filename>`.
    pub filename: String,
    /// Carried, not parsed from the filename: `meeting-20260907-0905` is a
    /// source id, so no split on a hyphen is safe.
    pub source: String,
    /// For [`Kind::EnrollSpeaker`] only: the stretches to embed, filled at
    /// lease time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spans: Vec<crate::enrol::Span>,
    /// For [`Kind::TranscribeSegment`] only: the language its session is pinned
    /// to (`crate::sessions::attach_language`); absent, the model guesses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

fn iso(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Derive the diarize job for every clip whose transcription succeeded: without
/// words to align, diarization attributes nothing. Idempotent and cheap enough
/// for every lease.
pub fn derive_jobs(conn: &Connection, now: DateTime<Utc>) -> rusqlite::Result<usize> {
    DERIVE_FOLLOW_ON.execute(
        conn,
        (Kind::DiarizeSegment, iso(now), Kind::TranscribeSegment),
    )
}

/// A filename without its extension: one recording can exist under two.
fn stem(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.')
        .map_or(name, |(base, _)| base)
        .to_owned()
}

/// Derive a transcription job, up to `limit`, for each microphone clip or
/// upload with no turns that the speech pass measured as not silent (a silent
/// minute comes back as "Thank you."). Undecodable (`UNKNOWN_SECONDS`) still
/// queues: the transcriber may read what VAD could not.
pub fn derive_segment_jobs(
    ingest: &Connection,
    meaning: &Connection,
    now: DateTime<Utc>,
    limit: usize,
) -> rusqlite::Result<usize> {
    // Read once: a correlated LIKE would scan both tables.
    let mut have: std::collections::HashSet<String> = std::collections::HashSet::new();
    {
        let mut stmt = TRANSCRIBED_PATHS.prepare(meaning)?;
        let rows = stmt.query_map([ROOM_SOURCE], |r| r.get::<_, String>(0))?;
        for path in rows {
            have.insert(stem(&path?));
        }
    }

    // A source the meaning plane does not know could never have its audio
    // registered, so it gets no job.
    let known: std::collections::HashSet<String> = {
        let mut stmt = NON_ROOM_SOURCES.prepare(meaning)?;
        let rows = stmt.query_map([crate::store::ROOM_KIND], |r| r.get::<_, String>(0))?;
        rows.collect::<Result<_, _>>()?
    };

    let candidates: Vec<(String, String, String)> = {
        let mut stmt = UNQUEUED_SPEECH.prepare(ingest)?;
        let rows = stmt.query_map((ROOM_SOURCE, Kind::TranscribeSegment, COPY_SECONDS), |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
        rows.collect::<Result<_, _>>()?
    };

    // Of two unqueued copies, the first stands for the minute.
    let mut taken: Vec<(String, DateTime<Utc>)> = Vec::new();
    let mut inserted = 0;
    for (filename, source, start) in candidates {
        if inserted >= limit {
            break;
        }
        if have.contains(&stem(&filename)) || !known.contains(&source) {
            continue;
        }
        let start = DateTime::parse_from_rfc3339(&start).map(|t| t.with_timezone(&Utc));
        if let Ok(start) = start {
            let copy = taken
                .iter()
                .any(|(s, t)| *s == source && (*t - start).num_seconds().abs() <= COPY_SECONDS);
            if copy {
                continue;
            }
            taken.push((source.clone(), start));
        }
        inserted += DERIVE_JOB.execute(ingest, (Kind::TranscribeSegment, &filename, iso(now)))?;
    }
    Ok(inserted)
}

/// Lease the newest queued or lapsed job of a kind in `kinds` (what the runner
/// can do: it holds one shim's weights). Empty `kinds` leases nothing.
pub fn lease(root: &Path, now: DateTime<Utc>, kinds: &[Kind]) -> rusqlite::Result<Option<Job>> {
    let conn = store::open(root)?;
    derive_jobs(&conn, now)?;
    retire_exhausted(&conn, now)?;
    // The kinds as a JSON array: SQLite binds no arrays.
    let kinds: Vec<&str> = kinds.iter().map(|k| k.as_str()).collect();
    let kinds = serde_json::to_string(&kinds)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
    let job: Option<Job> = LEASE
        .query_row(&conn, (iso(now), kinds, Kind::EnrollSpeaker), |r| {
            Ok(Job {
                id: r.get(0)?,
                kind: r.get(1)?,
                filename: r.get(2)?,
                source: r.get(3)?,
                // Filled by the caller, which can reach the meaning plane.
                spans: Vec::new(),
                language: None,
            })
        })
        .optional()?;
    if let Some(job) = &job {
        LEASE_JOB.execute(&conn, (iso(now + Duration::seconds(LEASE_TTL_S)), job.id))?;
    }
    Ok(job)
}

/// Retire every job whose leases are spent and lapsed, as a failure.
fn retire_exhausted(conn: &Connection, now: DateTime<Utc>) -> rusqlite::Result<usize> {
    RETIRE_EXHAUSTED.execute(
        conn,
        (
            iso(now),
            format!(r#"{{"ok":false,"error":"gave up after {MAX_ATTEMPTS} attempts"}}"#),
            MAX_ATTEMPTS,
        ),
    )
}

/// Retire a job with its result, opaque JSON the passes interpret.
pub fn done(root: &Path, id: i64, result: &str, now: DateTime<Utc>) -> rusqlite::Result<bool> {
    let conn = store::open(root)?;
    let updated = FINISH.execute(&conn, (iso(now), result, id))?;
    Ok(updated == 1)
}
