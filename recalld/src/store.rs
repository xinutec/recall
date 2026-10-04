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

crate::statements! {
    FORGET_SPEECH: Ingest =
        "DELETE FROM segment_speech WHERE filename IN (SELECT filename FROM segments WHERE source = ?1)";
    FORGET_LEVELS: Ingest =
        "DELETE FROM segment_levels WHERE filename IN (SELECT filename FROM segments WHERE source = ?1)";
    FORGET_REQUESTS: Ingest =
        "DELETE FROM retranscribe_requests WHERE filename IN (SELECT filename FROM segments WHERE source = ?1)";
    FORGET_JOBS: Ingest =
        "DELETE FROM jobs WHERE filename IN (SELECT filename FROM segments WHERE source = ?1)";
    FORGET_LEDGER: Ingest =
        "DELETE FROM pass_ledger WHERE filename IN (SELECT filename FROM segments WHERE source = ?1)";
    FORGET_CLIPS: Ingest = "DELETE FROM clips WHERE source = ?1";
    FORGET_SEGMENTS: Ingest = "DELETE FROM segments WHERE source = ?1";
    INSERT: Ingest =
        "INSERT INTO segments
             (filename, source, start_utc, bytes, sha256, received_utc, sent_utc)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";
    LOOKUP: Ingest =
        "SELECT source, filename, start_utc, bytes, sha256, received_utc, sent_utc
         FROM segments WHERE filename = ?1";
    // ?2 bounds the start (NULL: from the first). The source (?1) has a
    // statement of its own so it can reach its rows by `segments_source_start`.
    LIST: Ingest =
        "SELECT source, filename, start_utc, bytes, sha256, received_utc, sent_utc
         FROM segments WHERE start_utc >= COALESCE(?2, '')
         ORDER BY start_utc, filename LIMIT ?3";
    LIST_SOURCE: Ingest =
        "SELECT source, filename, start_utc, bytes, sha256, received_utc, sent_utc
         FROM segments WHERE source = ?1 AND start_utc >= COALESCE(?2, '')
         ORDER BY start_utc, filename LIMIT ?3";
}

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

/// The room stream's source and its meaning-plane kind. The stream was removed
/// (#1388); its old rows stay as history, and every pass here skips them.
pub const ROOM_SOURCE: &str = "room";
pub const ROOM_KIND: &str = "derived";

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

/// Remove every ingest-plane row of a deleted upload, in one transaction: its
/// transcription and diarization (the jobs), measurements, ledger, clips and
/// segment rows.
///
/// ⚠ The ingest plane is append-only for recorders; this is the one deletion,
/// and [`crate::sessions::DeletedUpload`] is its only key, so household capture
/// cannot reach it.
pub fn forget_upload(
    conn: &mut Connection,
    deleted: &crate::sessions::DeletedUpload,
) -> rusqlite::Result<()> {
    let tx = crate::sql::write(conn)?;
    let source = deleted.source();
    for statement in [
        FORGET_SPEECH,
        FORGET_LEVELS,
        FORGET_REQUESTS,
        FORGET_JOBS,
        FORGET_LEDGER,
        FORGET_CLIPS,
        FORGET_SEGMENTS,
    ] {
        statement.execute(&tx, [source])?;
    }
    tx.commit()
}

pub fn insert(conn: &Connection, row: &Row) -> rusqlite::Result<()> {
    INSERT.execute(
        conn,
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
    LOOKUP
        .query_row(conn, [filename], |r| {
            Ok(Row {
                source: r.get(0)?,
                filename: r.get(1)?,
                start_utc: r.get(2)?,
                bytes: byte_count(r, 3)?,
                sha256: r.get(4)?,
                received_utc: r.get(5)?,
                sent_utc: r.get(6)?,
            })
        })
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
    let sql = if source.is_some() { LIST_SOURCE } else { LIST };
    let mut stmt = sql.prepare(conn)?;
    let rows = stmt.query_map(rusqlite::params![source, since, limit], |r| {
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
