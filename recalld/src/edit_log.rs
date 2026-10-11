//! The edit log: what people did to the record, in order, in `edits`.
//! The only place that reads or writes the table.
//!
//! Append-only: a change of mind is an [`Act::Retract`] of its own. Each row
//! names the clip or session its act touches, so one clip's acts load by
//! index.

use crate::legacy_edits;
use rusqlite::{Connection, OptionalExtension};
use std::path::Path;
use transcript::{Act, Clip, Edit, EditId, Instant};

crate::statements! {
    APPEND: Meaning =
        "INSERT INTO edits (at_us, act, clip_id, source_id, origin) VALUES (?1, ?2, ?3, ?4, ?5)";
    TARGET: Meaning = "SELECT clip_id, source_id FROM edits WHERE id = ?1";
    /// The clip's own acts and its session's.
    FOR_CLIP: Meaning =
        "SELECT id, at_us, act FROM edits WHERE clip_id = ?1 OR source_id = ?2 ORDER BY id";
    ALL: Meaning = "SELECT id, at_us, act FROM edits ORDER BY id";
    COUNT: Meaning = "SELECT count(*) FROM edits";
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
    /// Today's tables could not be read for the seed.
    Legacy(String),
}

impl std::fmt::Display for LogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db(err) => write!(f, "{err}"),
            Self::Unreadable { id, why } => write!(f, "edit {id} does not read back: {why}"),
            Self::Unwritable(why) => write!(f, "act does not serialise: {why}"),
            Self::NoTarget(id) => write!(f, "no edit {} to retract", id.stored()),
            Self::Legacy(why) => write!(f, "cannot recover the edits: {why}"),
        }
    }
}

impl std::error::Error for LogError {}

impl From<rusqlite::Error> for LogError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Db(err)
    }
}

/// Append `act`, done at `at`. `origin` names the old row it was recovered
/// from, for the seed only.
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
        | Act::Unintelligible { clip, .. } => (Some(clip.stored()), None),
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

/// Fill an empty log from today's tables, dated as `legacy_edits::convert`
/// dates them, in one transaction. A log with any act is left alone. Returns
/// how many acts were written.
///
/// # Errors
/// If a store refuses or today's tables cannot be read.
pub fn seed(
    meaning: &Connection,
    ingest: &Connection,
    root: &Path,
    now: Instant,
) -> Result<usize, LogError> {
    let tx = crate::sql::write_shared(meaning)?;
    if COUNT.query_row(&tx, [], |r| r.get::<_, i64>(0))? > 0 {
        return Ok(0);
    }
    let loaded =
        legacy_edits::load(&tx, ingest, root).map_err(|err| LogError::Legacy(err.to_string()))?;
    let recovered = legacy_edits::convert(
        &loaded.corrections,
        &loaded.lines,
        &loaded.hides,
        &loaded.pins,
        now,
    );
    for (at, act, origin) in &recovered.acts {
        append(&tx, *at, act, Some(&format!("{origin:?}")))?;
    }
    tx.commit()?;
    Ok(recovered.acts.len())
}
