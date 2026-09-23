//! The ingest plane's bookkeeping: one row per stored blob, in
//! `<root>/ingest.sqlite`. The transcript system of record (`recall.sqlite`) is
//! a separate plane (docs/architecture.md, "recalld").
//!
//! Append-only: there is no delete here because no network path deletes
//! (decision 2).

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use std::path::Path;
use std::time::Duration;

/// A byte count column, read as the i64 SQLite stores and returned as u64. A
/// negative value is an out-of-range error naming the column.
fn byte_count(r: &rusqlite::Row<'_>, column: usize) -> rusqlite::Result<u64> {
    let stored: i64 = r.get(column)?;
    u64::try_from(stored).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(column, stored))
}

/// One stored segment, as the read side serves it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Row {
    pub source: String,
    pub filename: String,
    pub start_utc: String,
    pub bytes: u64,
    pub sha256: String,
    pub received_utc: String,
    /// The recorder's own clock at upload, when it sent one, which clock skew
    /// is measured against. Name-vs-arrival is delivery latency, not skew: a
    /// cached backlog arrives late legitimately.
    pub sent_utc: Option<String>,
}

/// Where a source's delivered blobs live: `<root>/ingest/<source>/`.
///
/// Use this rather than spelling the path out: nothing type-checks a `join`,
/// and a wrong one records unplayable audio paths.
#[must_use]
pub fn source_dir(root: &std::path::Path, source: &str) -> std::path::PathBuf {
    root.join("ingest").join(source)
}

pub fn open(root: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(root.join("ingest.sqlite"))?;
    conn.busy_timeout(Duration::from_secs(5))?;
    // WAL so the readers never block a recorder's upload, and vice versa.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    crate::ingest_schema::ensure(&conn)?;
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
            // SQLite's integer is i64; a file size fits.
            i64::try_from(row.bytes).expect("a byte count fits SQLite's i64"),
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
                bytes: byte_count(r, 3)?,
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
            bytes: byte_count(r, 3)?,
            sha256: r.get(4)?,
            received_utc: r.get(5)?,
            sent_utc: r.get(6)?,
        })
    })?;
    rows.collect()
}
