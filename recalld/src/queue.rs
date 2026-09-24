//! The work queue the Mac's runners poll.
//!
//! Jobs are derived from the blobs, never enqueued, so a missed enqueue cannot
//! strand audio: a `transcribe-segment` job exists for exactly every clip without
//! a completed one. Leases are time-bounded; a runner that dies lets its lapse and
//! the job is offered again. Newest clip first: "what are they saying now"
//! outranks backfill.

use crate::room::ROOM_SOURCE;
use crate::store;
use audiocore::job::Kind;
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use std::path::Path;

const LEASE_TTL_S: i64 = 10 * 60;
/// Leases a job may take before it is retired as failed. A runner that dies
/// mid-job lets its lease lapse and the job is offered again; a clip that kills
/// the shim every time would otherwise be offered every ten minutes for ever,
/// newest first, holding the GPU.
pub const MAX_ATTEMPTS: i64 = 3;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Job {
    pub id: i64,
    pub kind: Kind,
    /// The blob to work on, fetchable via `/ingest/v1/blob/<source>/<filename>`.
    pub filename: String,
    /// Which recorder it came from: the `<source>` in the blob URL. Carried, not
    /// derived: `meeting-20260907-0905` is a source id, and no split of a filename
    /// on a hyphen is safe.
    pub source: String,
    /// For [`Kind::EnrollSpeaker`] only: which stretches of the clip to embed. Filled at
    /// lease time from the meaning plane, so a voice renamed since the job was
    /// derived is embedded under its current name. Empty and omitted for every
    /// other kind.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spans: Vec<crate::enrol::Span>,
}

fn iso(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Derive queued jobs for room segments that have none. Idempotent; the
/// belt that makes a lost enqueue impossible.
pub fn derive_jobs(conn: &Connection, now: DateTime<Utc>) -> rusqlite::Result<usize> {
    // A segment measured silent gets no job: transcribing silence returns
    // inventions, not nothing. An unmeasured segment still gets one, because "not
    // looked at yet" is not evidence of silence.
    let inserted = conn.execute(
        "INSERT OR IGNORE INTO jobs (kind, filename, created_utc)
         SELECT ?1, s.filename, ?2 FROM segments s
         LEFT JOIN segment_speech p ON p.filename = s.filename
         WHERE s.source = ?3
           AND (p.filename IS NULL OR p.speech_seconds != 0.0)
           AND NOT EXISTS (SELECT 1 FROM jobs j
                           WHERE j.kind = ?1 AND j.filename = s.filename)",
        (Kind::TranscribeRoom, iso(now), ROOM_SOURCE),
    )?;
    Ok(inserted + derive_diarize_jobs(conn, now)? + derive_diarize_segment_jobs(conn, now)?)
}

/// A clip's identity without its container: the same recording can exist under
/// two extensions, so whole filenames compare unequal for one clip.
fn stem(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.')
        .map_or(name, |(base, _)| base)
        .to_owned()
}

/// Derive a transcribe job for each microphone clip or upload that has no turns
/// yet, bounded by `limit` so a backlog queues in bites rather than days of GPU
/// work in one statement.
///
/// The join across the planes is on the filename: the two planes spell the same
/// instant differently, and `start_utc` compared as text matches nothing.
pub fn derive_segment_jobs(
    ingest: &Connection,
    meaning: &Connection,
    now: DateTime<Utc>,
    limit: usize,
) -> rusqlite::Result<usize> {
    // The filenames that already have turns, read in one pass: a correlated LIKE
    // over both tables is a full scan of each.
    let mut have: std::collections::HashSet<String> = std::collections::HashSet::new();
    {
        let mut stmt = meaning.prepare(
            "SELECT DISTINCT a.path FROM audio_segments a
             JOIN transcript_segments t ON t.audio_segment_id = a.id
             WHERE a.source_id != ?1",
        )?;
        let rows = stmt.query_map([ROOM_SOURCE], |r| r.get::<_, String>(0))?;
        for path in rows {
            have.insert(stem(&path?));
        }
    }

    // Every source the meaning plane knows except the room, uploads included:
    // a source this plane has never heard of gets no job, because nothing could
    // register that clip's audio either, so the job could only go barren.
    let known: std::collections::HashSet<String> = {
        let mut stmt = meaning.prepare("SELECT id FROM sources WHERE kind != ?1")?;
        let rows = stmt.query_map([crate::room::ROOM_KIND], |r| r.get::<_, String>(0))?;
        rows.collect::<Result<_, _>>()?
    };

    let candidates: Vec<(String, String)> = {
        let mut stmt = ingest.prepare(
            "SELECT s.filename, s.source FROM segments s
             LEFT JOIN segment_speech p ON p.filename = s.filename
             WHERE s.source != ?1
               AND (p.filename IS NULL OR p.speech_seconds != 0.0)
               AND NOT EXISTS (SELECT 1 FROM jobs j
                               WHERE j.kind = ?2 AND j.filename = s.filename)
             ORDER BY s.start_utc DESC",
        )?;
        let rows = stmt.query_map((ROOM_SOURCE, Kind::TranscribeSegment), |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        rows.collect::<Result<_, _>>()?
    };

    let mut inserted = 0;
    for (filename, source) in candidates {
        if inserted >= limit {
            break;
        }
        if have.contains(&stem(&filename)) || !known.contains(&source) {
            continue;
        }
        inserted += ingest.execute(
            "INSERT OR IGNORE INTO jobs (kind, filename, created_utc) VALUES (?1, ?2, ?3)",
            (Kind::TranscribeSegment, &filename, iso(now)),
        )?;
    }
    Ok(inserted)
}

/// Derive a diarization job for every block whose transcription succeeded.
/// Diarization alone attributes nothing; it is the alignment against words that
/// makes turns, and a clip the ASR refused would only cost the GPU the same
/// answer again. Not gated on speech: a silent block never gets a transcription
/// job, so it cannot reach here.
fn derive_diarize_jobs(conn: &Connection, now: DateTime<Utc>) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT OR IGNORE INTO jobs (kind, filename, created_utc)
         SELECT ?1, j.filename, ?2 FROM jobs j
         WHERE j.kind = ?3 AND j.done_utc IS NOT NULL
           AND json_valid(j.result) AND json_extract(j.result, '$.ok') = 1
           AND NOT EXISTS (SELECT 1 FROM jobs d
                           WHERE d.kind = ?1 AND d.filename = j.filename)",
        (Kind::DiarizeRoom, iso(now), Kind::TranscribeRoom),
    )
}

/// The same, for one microphone's clip.
fn derive_diarize_segment_jobs(conn: &Connection, now: DateTime<Utc>) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT OR IGNORE INTO jobs (kind, filename, created_utc)
         SELECT ?1, j.filename, ?2 FROM jobs j
         WHERE j.kind = ?3 AND j.done_utc IS NOT NULL
           AND json_valid(j.result) AND json_extract(j.result, '$.ok') = 1
           AND NOT EXISTS (SELECT 1 FROM jobs d
                           WHERE d.kind = ?1 AND d.filename = j.filename)",
        (Kind::DiarizeSegment, iso(now), Kind::TranscribeSegment),
    )
}

/// Lease the newest available job of a kind the caller can do: queued, or
/// leased and lapsed. The lease is the only mutation; a runner acks by
/// finishing.
///
/// `kinds` is what the runner can do, not what exists: a runner holds one shim's
/// weights, and a job it cannot do would cycle through its attempts against a
/// process that can never do it. An empty `kinds` leases nothing.
pub fn lease(root: &Path, now: DateTime<Utc>, kinds: &[Kind]) -> rusqlite::Result<Option<Job>> {
    let conn = store::open(root)?;
    derive_jobs(&conn, now)?;
    retire_exhausted(&conn, now)?;
    // Built rather than bound: SQLite has no array binding, and numbered so the
    // placeholders read in order.
    let places = (2..=kinds.len() + 1)
        .map(|n| format!("?{n}"))
        .collect::<Vec<_>>()
        .join(", ");
    // Ordered by capture time through the join, not by filename: filename order
    // is source-alphabetical the moment a second source exists, and a runner would
    // drain every `usb` clip ever recorded before another source got a job. The
    // join is safe because every job is derived from a `segments` row.
    //
    // Enrolment outranks capture time. It is derived from what a person typed, on
    // whatever clip they were reading, usually old, and would otherwise wait behind
    // days of newer diarize jobs. Its derivation is bounded to a few per pass, so at
    // most a handful jump the queue.
    let sql = format!(
        "SELECT j.id, j.kind, j.filename, s.source FROM jobs j
         JOIN segments s ON s.filename = j.filename
         WHERE j.state IN ('queued', 'leased')
           AND (j.leased_until IS NULL OR j.leased_until < ?1)
           AND j.done_utc IS NULL
           AND j.kind IN ({places})
         ORDER BY (j.kind = '{enroll}') DESC, s.start_utc DESC, j.filename DESC
         LIMIT 1",
        enroll = Kind::EnrollSpeaker.as_str(),
    );
    let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(kinds.len() + 1);
    let stamp = iso(now);
    params.push(&stamp);
    for kind in kinds {
        params.push(kind);
    }
    let job: Option<Job> = conn
        .query_row(&sql, params.as_slice(), |r| {
            Ok(Job {
                id: r.get(0)?,
                kind: r.get(1)?,
                filename: r.get(2)?,
                source: r.get(3)?,
                // Filled by the caller that can reach the meaning plane; this
                // one holds only the ingest connection.
                spans: Vec::new(),
            })
        })
        .optional()?;
    if let Some(job) = &job {
        conn.execute(
            "UPDATE jobs SET state = 'leased', leased_until = ?1,
                             attempts = attempts + 1
             WHERE id = ?2",
            (iso(now + Duration::seconds(LEASE_TTL_S)), job.id),
        )?;
    }
    Ok(job)
}

/// Retire every job whose leases are spent and lapsed, as a failure the passes
/// ledger like any other refusal.
fn retire_exhausted(conn: &Connection, now: DateTime<Utc>) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE jobs SET state = 'done', done_utc = ?1, result = ?2
         WHERE done_utc IS NULL AND attempts >= ?3
           AND (leased_until IS NULL OR leased_until < ?1)",
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
    let updated = conn.execute(
        "UPDATE jobs SET state = 'done', done_utc = ?1, result = ?2
         WHERE id = ?3 AND done_utc IS NULL",
        (iso(now), result, id),
    )?;
    Ok(updated == 1)
}
