//! The meaning plane's `settings`: one text value per key, rewritten whole.

use rusqlite::{Connection, OptionalExtension};

crate::statements! {
    GET: Meaning = "SELECT value FROM settings WHERE key = ?1";
    SET: Meaning =
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value";
}

/// The stored value, or `None` when the key was never set.
///
/// # Errors
/// If the store refuses.
pub fn get(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    GET.query_row(conn, [key], |r| r.get(0)).optional()
}

/// # Errors
/// If the store refuses.
pub fn set(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    SET.execute(conn, [key, value])?;
    Ok(())
}
