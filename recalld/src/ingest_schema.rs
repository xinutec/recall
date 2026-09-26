//! The ingest plane's schema (`ingest.sqlite`), in one place.
//!
//! `CREATE TABLE IF NOT EXISTS` reaches a new database only; a column added
//! later has to be added to the tables that already exist, which is what the
//! `ALTER`s below are for. Every open goes through this, so no pass can meet a
//! table another pass was meant to have created.

use rusqlite::Connection;

pub fn ensure(conn: &Connection) -> rusqlite::Result<()> {
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
             ON segments (source, start_utc);

         CREATE TABLE IF NOT EXISTS segment_speech (
             filename       TEXT PRIMARY KEY REFERENCES segments (filename),
             source         TEXT NOT NULL,
             speech_seconds REAL NOT NULL,
             computed_utc   TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS segment_speech_source
             ON segment_speech (source, filename);

         CREATE TABLE IF NOT EXISTS segment_levels (
             filename     TEXT PRIMARY KEY REFERENCES segments (filename),
             source       TEXT NOT NULL,
             speech_db    REAL NOT NULL,
             floor_db     REAL NOT NULL,
             -- NULL means measured before the column existed: unknown, never zero.
             gated        REAL,
             quiet_run_s  REAL,
             computed_utc TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS segment_levels_source
             ON segment_levels (source, filename);

         CREATE TABLE IF NOT EXISTS room_blocks (
             start_utc    TEXT PRIMARY KEY,
             verdict      TEXT NOT NULL,
             winner       TEXT,
             filename     TEXT,
             contributors TEXT NOT NULL,
             -- NULL means judged before coverage was measured, not fully covered.
             coverage     REAL,
             built_utc    TEXT NOT NULL
         );

         CREATE TABLE IF NOT EXISTS jobs (
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
         );

         -- Clips a pass decided without writing, keyed on (kind, filename)
         -- because three passes share it and reach the same clip by name.
         CREATE TABLE IF NOT EXISTS pass_ledger (
             kind        TEXT NOT NULL,
             filename    TEXT NOT NULL,
             outcome     TEXT NOT NULL,
             decided_utc TEXT NOT NULL,
             PRIMARY KEY (kind, filename)
         );",
    )?;
    add_column(conn, "segment_levels", "gated", "REAL")?;
    add_column(conn, "segment_levels", "quiet_run_s", "REAL")?;
    add_column(conn, "room_blocks", "coverage", "REAL")?;
    // Where the speech is, as JSON `[[start, end], ...]` in seconds from the
    // blob's start; JSON `null` when it could not be decoded. NULL means not
    // looked for yet (`speech::scan_once` backfills it).
    add_column(conn, "segment_speech", "regions", "TEXT")?;
    // A decision's counts, as JSON; `outcome` is one word (`ledger::Outcome`).
    add_column(conn, "pass_ledger", "detail", "TEXT")?;
    split_old_outcomes(conn)?;
    retire_room_jobs(conn)
}

/// Rows written before `detail` put their counts in the outcome as a sentence
/// ("attributed: 10 turn(s) named in place ..."). The word stays the outcome;
/// the whole sentence moves to `detail`, so nothing is lost.
///
/// Read first, like [`retire_room_jobs`]: every open runs this.
fn split_old_outcomes(conn: &Connection) -> rusqlite::Result<()> {
    let old: bool = conn
        .prepare("SELECT 1 FROM pass_ledger WHERE instr(outcome, ':') > 0")?
        .exists([])?;
    if old {
        conn.execute(
            "UPDATE pass_ledger
                SET detail = json_object('was', outcome),
                    outcome = substr(outcome, 1, instr(outcome, ':') - 1)
              WHERE instr(outcome, ':') > 0",
            [],
        )?;
    }
    Ok(())
}

/// The room stream left production (`experimental/room`), and its job kinds
/// with it. Its unfinished jobs are closed as failures, not deleted, so the
/// queue keeps its history.
///
/// Read first: an UPDATE takes the write lock even when it matches nothing, and
/// every open runs this.
fn retire_room_jobs(conn: &Connection) -> rusqlite::Result<()> {
    let kinds = ("transcribe-room", "diarize-room");
    let open: bool = conn
        .prepare("SELECT 1 FROM jobs WHERE kind IN (?1, ?2) AND done_utc IS NULL")?
        .exists(kinds)?;
    if open {
        conn.execute(
            r#"UPDATE jobs SET state = 'done',
                   done_utc = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
                   result = '{"ok":false,"error":"room stream retired"}'
               WHERE kind IN (?1, ?2) AND done_utc IS NULL"#,
            kinds,
        )?;
    }
    Ok(())
}

/// Add a column to a table that already exists. Every argument is a literal
/// from this file, never input.
fn add_column(conn: &Connection, table: &str, name: &str, ty: &str) -> rusqlite::Result<()> {
    let present: bool = conn
        .prepare(&format!(
            "SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1"
        ))?
        .exists([name])?;
    if present {
        return Ok(());
    }
    conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {name} {ty}"))
}
