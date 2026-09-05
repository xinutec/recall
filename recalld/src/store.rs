//! The ingest plane's bookkeeping: one row per stored blob, in
//! `<root>/ingest.sqlite`. This database belongs to recalld alone — the
//! transcript system of record (`recall.sqlite`) is a different plane and a
//! different writer, the same audio/meaning split the Mac keeps
//! (docs/architecture.md, "recalld").
//!
//! Append-only by construction: there is no delete here because no network
//! path deletes (decision 2). Retention (stage D) will be the one writer that
//! ever removes anything, and it arrives with its own evidence trail.

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use std::path::Path;
use std::time::Duration;

/// One stored segment, as the read side serves it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Row {
    pub source: String,
    pub filename: String,
    pub start_utc: String,
    pub bytes: u64,
    pub sha256: String,
    pub received_utc: String,
    /// The recorder's own clock at upload, when it sent one — what clock skew
    /// is measured against. Name-vs-arrival is delivery latency, not skew: a
    /// cached backlog arrives late legitimately.
    pub sent_utc: Option<String>,
}

pub fn open(root: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(root.join("ingest.sqlite"))?;
    conn.busy_timeout(Duration::from_secs(5))?;
    // WAL so the eventual readers (room builder, retention) never block a
    // recorder's upload, and vice versa.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS segments (
             filename     TEXT PRIMARY KEY,
             source       TEXT NOT NULL,
             start_utc    TEXT NOT NULL,
             bytes        INTEGER NOT NULL,
             sha256       TEXT NOT NULL,
             received_utc TEXT NOT NULL,
             sent_utc     TEXT
         );
         CREATE INDEX IF NOT EXISTS segments_source_start
             ON segments (source, start_utc);",
    )?;
    Ok(conn)
}

pub fn insert(conn: &Connection, row: &Row) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO segments
             (filename, source, start_utc, bytes, sha256, received_utc, sent_utc)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        (
            &row.filename,
            &row.source,
            &row.start_utc,
            row.bytes,
            &row.sha256,
            &row.received_utc,
            &row.sent_utc,
        ),
    )?;
    Ok(())
}

pub fn lookup(conn: &Connection, filename: &str) -> rusqlite::Result<Option<Row>> {
    conn.query_row(
        "SELECT source, filename, start_utc, bytes, sha256, received_utc, sent_utc
         FROM segments WHERE filename = ?1",
        [filename],
        |r| {
            Ok(Row {
                source: r.get(0)?,
                filename: r.get(1)?,
                start_utc: r.get(2)?,
                bytes: r.get(3)?,
                sha256: r.get(4)?,
                received_utc: r.get(5)?,
                sent_utc: r.get(6)?,
            })
        },
    )
    .optional()
}

/// The read side's listing: everything, one source's, or one source's since
/// an instant — ordered by capture start so a consumer walks time forward.
/// Each source's newest CAPTURE time (`start_utc`, taken from the segment name),
/// deliberately NOT its arrival time: a cached backlog draining hours late
/// arrives now and would read as "recording now" while proving nothing about
/// now. Index-served by `segments_source_start`.
pub fn liveness(conn: &Connection) -> rusqlite::Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare("SELECT source, MAX(start_utc) FROM segments GROUP BY source")?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect()
}

pub fn list(
    conn: &Connection,
    source: Option<&str>,
    since: Option<&str>,
    limit: u32,
) -> rusqlite::Result<Vec<Row>> {
    let mut sql = String::from(
        "SELECT source, filename, start_utc, bytes, sha256, received_utc, sent_utc
         FROM segments WHERE 1=1",
    );
    let mut params: Vec<&dyn rusqlite::ToSql> = Vec::new();
    if let Some(source) = source.as_ref() {
        sql.push_str(" AND source = ?");
        params.push(source);
    }
    if let Some(since) = since.as_ref() {
        sql.push_str(" AND start_utc >= ?");
        params.push(since);
    }
    sql.push_str(" ORDER BY start_utc, filename LIMIT ?");
    params.push(&limit);
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params.as_slice(), |r| {
        Ok(Row {
            source: r.get(0)?,
            filename: r.get(1)?,
            start_utc: r.get(2)?,
            bytes: r.get(3)?,
            sha256: r.get(4)?,
            received_utc: r.get(5)?,
            sent_utc: r.get(6)?,
        })
    })?;
    rows.collect()
}
