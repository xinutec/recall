//! The edit log: what people did to the record, in order, in `edits`.
//! The only place that reads or writes the table.
//!
//! Append-only: a change of mind is an [`Act::Retract`] of its own. Each row
//! names the clip or session its act touches, so one clip's acts load by
//! index.

use rusqlite::{Connection, OptionalExtension};
use transcript::{Act, Clip, Edit, EditId, Instant};

crate::statements! {
    APPEND: Meaning =
        "INSERT INTO edits (at_us, act, clip_id, source_id, origin) VALUES (?1, ?2, ?3, ?4, ?5)";
    TARGET: Meaning = "SELECT clip_id, source_id FROM edits WHERE id = ?1";
    /// The clip's own acts and its session's.
    FOR_CLIP: Meaning =
        "SELECT id, at_us, act FROM edits WHERE clip_id = ?1 OR source_id = ?2 ORDER BY id";
    ALL: Meaning = "SELECT id, at_us, act FROM edits ORDER BY id";
    /// A session's own acts and their retractions.
    FOR_SOURCE: Meaning = "SELECT id, at_us, act FROM edits WHERE source_id = ?1 ORDER BY id";
    ONE: Meaning = "SELECT id, at_us, act FROM edits WHERE id = ?1";
    /// The acts filed in one write: they share its instant.
    SAME_AT: Meaning = "SELECT id, at_us, act FROM edits WHERE at_us = ?1 ORDER BY id";
    RECOVERED: Meaning = "SELECT id FROM edits WHERE origin IS NOT NULL";
}

#[derive(Debug)]
pub enum LogError {
    Db(rusqlite::Error),
    /// A stored row that is not an act: the log is the record, so this stops
    /// the read rather than being skipped.
    Unreadable {
        id: i64,
        why: String,
    },
    /// An act that would not serialise.
    Unwritable(String),
    /// A retraction of an edit the log does not hold.
    NoTarget(EditId),
}

impl std::fmt::Display for LogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db(err) => write!(f, "{err}"),
            Self::Unreadable { id, why } => write!(f, "edit {id} does not read back: {why}"),
            Self::Unwritable(why) => write!(f, "act does not serialise: {why}"),
            Self::NoTarget(id) => write!(f, "no edit {} to retract", id.stored()),
        }
    }
}

impl std::error::Error for LogError {}

impl From<rusqlite::Error> for LogError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Db(err)
    }
}

/// Append `act`, done at `at`. `origin` names the old row an act was
/// recovered from; the app's own acts have none.
///
/// # Errors
/// If the store refuses, or a retraction names no stored edit.
pub fn append(
    conn: &Connection,
    at: Instant,
    act: &Act,
    origin: Option<&str>,
) -> Result<EditId, LogError> {
    let (clip, source) = match act {
        Act::Words { clip, .. }
        | Act::Speaker { clip, .. }
        | Act::NoSpeech { clip, .. }
        | Act::Unintelligible { clip, .. }
        | Act::Exclude { clip, .. } => (Some(clip.stored()), None),
        Act::Voice { source, .. } | Act::Language { source, .. } => {
            (None, Some(source.as_str().to_owned()))
        }
        Act::Retract { edit } => TARGET
            .query_row(conn, [edit.stored()], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?
            .ok_or(LogError::NoTarget(*edit))?,
    };
    let json = serde_json::to_string(act).map_err(|err| LogError::Unwritable(err.to_string()))?;
    APPEND.execute(conn, (at.micros(), json, clip, source, origin))?;
    Ok(EditId::from_stored(conn.last_insert_rowid()))
}

/// The acts that can change `clip`'s lines: its own and its session's, in
/// order.
///
/// # Errors
/// If the store refuses or a row does not read back.
pub fn for_clip(conn: &Connection, clip: &Clip) -> Result<Vec<Edit>, LogError> {
    read(
        FOR_CLIP.prepare(conn)?,
        (clip.id.stored(), clip.source.as_str()),
    )
}

/// A session's own acts (voice names, language), in order.
///
/// # Errors
/// If the store refuses or a row does not read back.
pub fn for_source(conn: &Connection, source: &str) -> Result<Vec<Edit>, LogError> {
    read(FOR_SOURCE.prepare(conn)?, [source])
}

/// One act by id.
///
/// # Errors
/// If the store refuses or the row does not read back.
pub fn get(conn: &Connection, id: EditId) -> Result<Option<Edit>, LogError> {
    Ok(read(ONE.prepare(conn)?, [id.stored()])?.pop())
}

/// Every act, in order.
///
/// # Errors
/// If the store refuses or a row does not read back.
pub fn all(conn: &Connection) -> Result<Vec<Edit>, LogError> {
    read(ALL.prepare(conn)?, ())
}

fn read(
    mut stmt: rusqlite::Statement<'_>,
    params: impl rusqlite::Params,
) -> Result<Vec<Edit>, LogError> {
    let rows: Vec<(i64, i64, String)> = stmt
        .query_map(params, |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    rows.into_iter()
        .map(|(id, at, json)| {
            let unreadable = |why: String| LogError::Unreadable { id, why };
            Ok(Edit {
                id: EditId::from_stored(id),
                at: Instant::from_micros(at).ok_or_else(|| unreadable("instant".to_owned()))?,
                act: serde_json::from_str(&json).map_err(|err| unreadable(err.to_string()))?,
            })
        })
        .collect()
}

/// The acts filed together with `edit`, itself included: one write's acts
/// share its instant.
///
/// # Errors
/// If the store refuses, a row does not read, or there is no such edit.
pub fn filed_with(conn: &Connection, edit: EditId) -> Result<Vec<Edit>, LogError> {
    let one = get(conn, edit)?.ok_or(LogError::NoTarget(edit))?;
    read(SAME_AT.prepare(conn)?, [one.at.micros()])
}

/// The acts recovered from the old tables, not done in the app.
///
/// # Errors
/// If the store refuses.
pub fn recovered(conn: &Connection) -> rusqlite::Result<std::collections::HashSet<EditId>> {
    RECOVERED
        .prepare(conn)?
        .query_map([], |r| Ok(EditId::from_stored(r.get(0)?)))?
        .collect()
}
