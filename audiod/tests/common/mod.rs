//! Shared fixture: the two tables audiod writes, DDL copied from
//! `store_schema.py` (sources as of the migration that added `port`). Real
//! shape, not a convenience one — the daemon must work against the actual
//! schema.

use std::path::Path;

pub fn create_schema(root: &Path) {
    let conn = rusqlite::Connection::open(root.join("recall.sqlite")).unwrap();
    conn.execute_batch(
        "CREATE TABLE sources (
             id   TEXT PRIMARY KEY,
             name TEXT NOT NULL,
             kind TEXT NOT NULL,
             port INTEGER
         );
         CREATE TABLE capture_events (
             id INTEGER PRIMARY KEY,
             utc TEXT NOT NULL,
             kind TEXT NOT NULL,
             source_id TEXT,
             detail TEXT
         );",
    )
    .unwrap();
}
