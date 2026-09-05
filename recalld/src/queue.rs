//! Stage E1 (docs/architecture.md): the work queue the Mac's runner polls.
//!
//! Jobs are DERIVED, not enqueued — the share-upload lesson: a
//! `transcribe-room` job exists for exactly every room segment without a
//! completed one, so a missed enqueue cannot strand audio. Leases are
//! time-bounded: a runner that dies mid-job simply lets its lease lapse and
//! the job is offered again. Newest room segment first — "what are they
//! saying now" outranks backfill (decision 8).

use crate::room::ROOM_SOURCE;
use crate::store;
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use std::path::Path;

/// The one job kind stage E starts with; refine/ask arrive at E4.
pub const TRANSCRIBE_ROOM: &str = "transcribe-room";
const LEASE_TTL_S: i64 = 10 * 60;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Job {
    pub id: i64,
    pub kind: String,
    /// The blob to work on, fetchable via `/ingest/v1/blob/room/<filename>`.
    pub filename: String,
}

pub fn ensure_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS jobs (
             id           INTEGER PRIMARY KEY,
             kind         TEXT NOT NULL,
             filename     TEXT NOT NULL,
             state        TEXT NOT NULL DEFAULT 'queued',
             leased_until TEXT,
             attempts     INTEGER NOT NULL DEFAULT 0,
             created_utc  TEXT NOT NULL,
             done_utc     TEXT,
             result       TEXT,
             UNIQUE (kind, filename)
         );",
    )
}

fn iso(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Derive queued jobs for room segments that have none. Idempotent; the
/// belt that makes a lost enqueue impossible.
pub fn derive_jobs(conn: &Connection, now: DateTime<Utc>) -> rusqlite::Result<usize> {
    let inserted = conn.execute(
        "INSERT OR IGNORE INTO jobs (kind, filename, created_utc)
         SELECT ?1, s.filename, ?2 FROM segments s
         WHERE s.source = ?3
           AND NOT EXISTS (SELECT 1 FROM jobs j
                           WHERE j.kind = ?1 AND j.filename = s.filename)",
        (TRANSCRIBE_ROOM, iso(now), ROOM_SOURCE),
    )?;
    Ok(inserted)
}

/// Lease the newest available job: queued, or leased-but-lapsed. The lease is
/// the only mutation — a runner acks by finishing, never by holding on.
pub fn lease(root: &Path, now: DateTime<Utc>) -> rusqlite::Result<Option<Job>> {
    let conn = store::open(root)?;
    ensure_schema(&conn)?;
    derive_jobs(&conn, now)?;
    let job: Option<Job> = conn
        .query_row(
            "SELECT id, kind, filename FROM jobs
             WHERE state IN ('queued', 'leased')
               AND (leased_until IS NULL OR leased_until < ?1)
               AND done_utc IS NULL
             ORDER BY filename DESC LIMIT 1",
            [iso(now)],
            |r| {
                Ok(Job {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    filename: r.get(2)?,
                })
            },
        )
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

/// Retire a job with its result (opaque JSON the E3 stage will interpret;
/// stored so nothing is lost while that lands).
pub fn done(root: &Path, id: i64, result: &str, now: DateTime<Utc>) -> rusqlite::Result<bool> {
    let conn = store::open(root)?;
    ensure_schema(&conn)?;
    let updated = conn.execute(
        "UPDATE jobs SET state = 'done', done_utc = ?1, result = ?2
         WHERE id = ?3 AND done_utc IS NULL",
        (iso(now), result, id),
    )?;
    Ok(updated == 1)
}
