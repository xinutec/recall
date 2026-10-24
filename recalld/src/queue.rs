//! The work queue the Mac's runners poll.
//!
//! Jobs are derived from the blobs, never enqueued, so a missed enqueue cannot
//! strand audio. Leases are time-bounded: a dead runner's job is offered
//! again. Newest clip first: what is being said now outranks backfill.

use crate::same_speech::COPY_SECONDS;
use crate::store::{self, ROOM_SOURCE};
use audiocore::instant::IngestStamp;
use audiocore::job::Kind;
use audiocore::work::Job;
use chrono::{DateTime, Duration, Utc};
use rusqlite::{Connection, OptionalExtension};
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
    /// Finished `?2` jobs that succeeded with no `?1` job yet, and only then
    /// their results: reading every result costs 25 s on a cold disk. A read:
    /// as an `INSERT … SELECT` it held the write lock across the scan, and
    /// after a reboot refused every lease for five minutes.
    FOLLOW_ONS_MISSING: Ingest =
        "WITH open AS MATERIALIZED (
             SELECT j.id FROM jobs j
             WHERE j.kind = ?2 AND j.done_utc IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM jobs d
                               WHERE d.kind = ?1 AND d.filename = j.filename))
         SELECT j.filename FROM jobs j JOIN open o ON o.id = j.id
         WHERE json_valid(j.result) AND json_extract(j.result, '$.ok') = 1";
    /// The same for one job just finished.
    FOLLOW_ON_OF: Ingest =
        "INSERT OR IGNORE INTO jobs (kind, filename, created_utc)
         SELECT ?1, filename, ?2 FROM jobs
         WHERE id = ?3 AND kind = ?4
           AND json_valid(result) AND json_extract(result, '$.ok') = 1";
    /// Clips transcribed before the jobs table, which have no job.
    TRANSCRIBED_PATHS: Meaning =
        "SELECT a.path FROM audio_segments a
             JOIN transcribed_before_jobs t ON t.audio_segment_id = a.id
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
        "UPDATE jobs SET state = 'done', done_utc = ?1, result = COALESCE(result, ?2)
         WHERE done_utc IS NULL AND attempts >= ?3
           AND (leased_until IS NULL OR leased_until < ?1)";
    RENEW: Ingest =
        "UPDATE jobs SET leased_until = ?1 WHERE id = ?2 AND done_utc IS NULL";
    FINISH: Ingest =
        "UPDATE jobs SET state = 'done', done_utc = ?1, result = ?2
         WHERE id = ?3 AND done_utc IS NULL";
}

/// A runner renews well inside this while it works (`renew`), so it bounds
/// only how long a crashed runner's job waits to be offered again.
const LEASE_TTL_S: i64 = 10 * 60;
/// Leases a job may take before it is retired as failed; otherwise a clip that
/// kills the shim would hold the GPU for ever.
pub const MAX_ATTEMPTS: i64 = 3;

/// What a successful transcription is followed by: diarization (without words
/// to align it attributes nothing) and each segment's own print.
const FOLLOW_ONS: [Kind; 2] = [Kind::DiarizeSegment, Kind::EmbedLines];

/// Derive the follow-on jobs for every clip whose transcription succeeded and
/// has none. [`done`] derives each as its transcription lands; this catches up
/// on any it missed, and on the whole archive when a kind is new. Idempotent.
/// Not on the lease path: it reads results.
pub fn derive_follow_ons(conn: &Connection, now: DateTime<Utc>) -> rusqlite::Result<usize> {
    let mut derived = 0;
    for kind in FOLLOW_ONS {
        let missing: Vec<String> = {
            let mut stmt = FOLLOW_ONS_MISSING.prepare(conn)?;
            let rows = stmt.query_map((kind, Kind::TranscribeSegment), |r| r.get(0))?;
            rows.collect::<Result<_, _>>()?
        };
        for filename in missing {
            derived += DERIVE_JOB.execute(conn, (kind, &filename, IngestStamp::of(now)))?;
        }
    }
    Ok(derived)
}

/// Derive a transcription job, up to `limit`, for each microphone clip or
/// upload never transcribed that the speech pass measured as not silent (a silent
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
            have.insert(crate::clips::stem(&path?).to_owned());
        }
    }

    // A source the meaning plane does not know could never have its audio
    // registered, so it gets no job.
    let known: std::collections::HashSet<String> = {
        let mut stmt = NON_ROOM_SOURCES.prepare(meaning)?;
        let rows = stmt.query_map([crate::sources::SourceKind::Derived], |r| {
            r.get::<_, String>(0)
        })?;
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
        if have.contains(crate::clips::stem(&filename)) || !known.contains(&source) {
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
        inserted += DERIVE_JOB.execute(
            ingest,
            (Kind::TranscribeSegment, &filename, IngestStamp::of(now)),
        )?;
    }
    Ok(inserted)
}

/// Lease the newest queued or lapsed job of a kind in `kinds` (what the runner
/// can do: it holds one shim's weights). Empty `kinds` leases nothing.
pub fn lease(root: &Path, now: DateTime<Utc>, kinds: &[Kind]) -> rusqlite::Result<Option<Job>> {
    lease_on(&mut store::open(root)?, now, kinds)
}

/// [`lease`] on an open ingest plane.
///
/// # Errors
/// If the store refuses.
pub fn lease_on(
    conn: &mut Connection,
    now: DateTime<Utc>,
    kinds: &[Kind],
) -> rusqlite::Result<Option<Job>> {
    // One short write: two runners must not lease the same job.
    let tx = crate::sql::write(conn)?;
    let conn = &*tx;
    retire_exhausted(conn, now)?;
    // The kinds as a JSON array: SQLite binds no arrays.
    let kinds: Vec<&str> = kinds.iter().map(|k| k.as_str()).collect();
    let kinds = serde_json::to_string(&kinds)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
    let job: Option<Job> = LEASE
        .query_row(
            conn,
            (IngestStamp::of(now), kinds, Kind::EnrollSpeaker),
            |r| {
                Ok(Job {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    filename: r.get(2)?,
                    source: r.get(3)?,
                    // Filled by the caller, which can reach the meaning plane.
                    spans: Vec::new(),
                    language: None,
                })
            },
        )
        .optional()?;
    if let Some(job) = &job {
        LEASE_JOB.execute(
            conn,
            (
                IngestStamp::of(now + Duration::seconds(LEASE_TTL_S)),
                job.id,
            ),
        )?;
    }
    tx.commit()?;
    Ok(job)
}

/// Hold a job that is still being worked on for another lease's length.
/// Returns whether it was open to hold.
///
/// # Errors
/// If the store refuses.
pub fn renew(root: &Path, id: i64, now: DateTime<Utc>) -> rusqlite::Result<bool> {
    let conn = store::open(root)?;
    Ok(RENEW.execute(
        &conn,
        (IngestStamp::of(now + Duration::seconds(LEASE_TTL_S)), id),
    )? == 1)
}

/// Retire every job whose leases are spent and lapsed, as a failure.
fn retire_exhausted(conn: &Connection, now: DateTime<Utc>) -> rusqlite::Result<usize> {
    RETIRE_EXHAUSTED.execute(
        conn,
        (
            IngestStamp::of(now),
            format!(r#"{{"ok":false,"error":"gave up after {MAX_ATTEMPTS} attempts"}}"#),
            MAX_ATTEMPTS,
        ),
    )
}

/// Retire a job with its result, opaque JSON the passes interpret.
/// A finished transcription queues its follow-ons in the same write.
pub fn done(root: &Path, id: i64, result: &str, now: DateTime<Utc>) -> rusqlite::Result<bool> {
    let mut conn = store::open(root)?;
    let tx = crate::sql::write(&mut conn)?;
    let updated = FINISH.execute(&tx, (IngestStamp::of(now), result, id))?;
    if updated == 1 {
        for kind in FOLLOW_ONS {
            FOLLOW_ON_OF.execute(
                &tx,
                (kind, IngestStamp::of(now), id, Kind::TranscribeSegment),
            )?;
        }
    }
    tx.commit()?;
    Ok(updated == 1)
}
