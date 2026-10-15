//! Clip identity (#1911): the one place that reads and writes `clips` in
//! `ingest.sqlite`, and the only constructor of a [`ClipId`] from storage.
//!
//! A stored file gets its clip from the database itself (the
//! `segments_have_clips` trigger, `ingest_schema`).

use rusqlite::{Connection, OptionalExtension};
use std::collections::HashMap;
use std::path::Path;
use transcript::{Clip, ClipId, Instant, SourceId};

crate::statements! {
    BY_FILENAME: Ingest =
        "SELECT id, source, start_us, filename, path FROM clips WHERE filename = ?1";
    BY_ID: Ingest =
        "SELECT id, source, start_us, filename, path FROM clips WHERE id = ?1";
    ALL: Ingest = "SELECT id, source, start_us, filename, path FROM clips";
    OF_SOURCE: Ingest =
        "SELECT id, source, start_us, filename, path FROM clips WHERE source = ?1";
    BY_PATH: Ingest =
        "SELECT id, source, start_us, filename, path FROM clips WHERE path = ?1";
    /// `?1`: filenames as JSON.
    FORGET_FILES: Ingest = "DELETE FROM clips WHERE filename IN (SELECT value FROM json_each(?1))";
    CENSUS_CLIPS: Ingest = "SELECT count(*) FROM clips";
    CENSUS_SEGMENTS_WITHOUT: Ingest =
        "SELECT count(*) FROM segments s WHERE NOT EXISTS (SELECT 1 FROM clips c WHERE c.filename = s.filename)";
    CENSUS_SHARED_STARTS: Ingest =
        "SELECT count(*) FROM clips c WHERE EXISTS
             (SELECT 1 FROM clips o WHERE o.source = c.source AND o.start_us = c.start_us AND o.id <> c.id)";
    AUDIO_PATHS: Meaning = "SELECT path FROM audio_segments";
}

/// A stored row that is not a clip the domain can hold.
#[derive(Debug)]
pub enum ClipError {
    Db(rusqlite::Error),
    /// The stored source fails the source grammar.
    Source(String),
    /// The stored start is outside the representable range.
    Start(i64),
}

impl std::fmt::Display for ClipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db(err) => write!(f, "{err}"),
            Self::Source(source) => write!(f, "stored source {source:?} is not a source id"),
            Self::Start(us) => write!(f, "stored start {us} is not an instant"),
        }
    }
}

impl std::error::Error for ClipError {}

impl From<rusqlite::Error> for ClipError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Db(err)
    }
}

type Raw = (i64, String, i64, String, String);

fn raw(r: &rusqlite::Row<'_>) -> rusqlite::Result<Raw> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
}

fn clip((id, source, start_us, filename, path): Raw) -> Result<Clip, ClipError> {
    Ok(Clip {
        id: ClipId::from_stored(id),
        source: SourceId::parse(&source).ok_or(ClipError::Source(source))?,
        start: Instant::from_micros(start_us).ok_or(ClipError::Start(start_us))?,
        filename,
        path,
    })
}

pub fn by_filename(ingest: &Connection, filename: &str) -> Result<Option<Clip>, ClipError> {
    BY_FILENAME
        .query_row(ingest, [filename], raw)
        .optional()?
        .map(clip)
        .transpose()
}

pub fn by_id(ingest: &Connection, id: ClipId) -> Result<Option<Clip>, ClipError> {
    BY_ID
        .query_row(ingest, [id.stored()], raw)
        .optional()?
        .map(clip)
        .transpose()
}

/// Every clip.
pub fn all(ingest: &Connection) -> Result<Vec<Clip>, ClipError> {
    ALL.prepare(ingest)?
        .query_map([], raw)?
        .map(|row| clip(row?))
        .collect()
}

/// A source's clips.
pub fn of_source(ingest: &Connection, source: &str) -> Result<Vec<Clip>, ClipError> {
    OF_SOURCE
        .prepare(ingest)?
        .query_map([source], raw)?
        .map(|row| clip(row?))
        .collect()
}

/// The clip a meaning-plane audio row names, by its absolute `path` under `root`.
pub fn for_audio_path(
    ingest: &Connection,
    root: &Path,
    path: &str,
) -> Result<Option<Clip>, ClipError> {
    let Some(relative) = relative(root, path) else {
        return Ok(None);
    };
    BY_PATH
        .query_row(ingest, [relative], raw)
        .optional()?
        .map(clip)
        .transpose()
}

/// Every clip by the absolute path a meaning-plane audio row names it by.
///
/// # Errors
/// If the store refuses or a stored clip does not read.
pub fn by_audio_path(ingest: &Connection, root: &Path) -> Result<HashMap<String, Clip>, ClipError> {
    Ok(all(ingest)?
        .into_iter()
        .map(|c| (root.join(&c.path).to_string_lossy().into_owned(), c))
        .collect())
}

/// A recording's name without its container. Rows from before clips name a
/// recording by a path whose extension can differ from the stored file's
/// (`.wav` against `.opus`); this is how they are matched.
pub fn stem(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').map_or(name, |(base, _)| base)
}

fn relative<'a>(root: &Path, path: &'a str) -> Option<&'a str> {
    path.strip_prefix(root.to_str()?)?.strip_prefix('/')
}

/// Delete the clips of these files, in the caller's transaction
/// ([`crate::store::forget_files`]).
pub(crate) fn forget_files(ingest: &Connection, filenames_json: &str) -> rusqlite::Result<usize> {
    FORGET_FILES.execute(ingest, [filenames_json])
}

/// Whether every stored file and every meaning-plane audio row has a clip.
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Census {
    pub clips: i64,
    pub files_without_clip: i64,
    pub audio_rows: usize,
    pub audio_rows_without_clip: usize,
    /// Clips sharing a source and a start second with another (copies).
    pub clips_sharing_a_start: i64,
}

pub fn census(meaning: &Connection, ingest: &Connection, root: &Path) -> Result<Census, ClipError> {
    let count = |sql: crate::sql::Sql| sql.query_row(ingest, [], |r| r.get::<_, i64>(0));
    let paths: Vec<String> = AUDIO_PATHS
        .prepare(meaning)?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut without = 0;
    for path in &paths {
        if for_audio_path(ingest, root, path)?.is_none() {
            without += 1;
        }
    }
    Ok(Census {
        clips: count(CENSUS_CLIPS)?,
        files_without_clip: count(CENSUS_SEGMENTS_WITHOUT)?,
        audio_rows: paths.len(),
        audio_rows_without_clip: without,
        clips_sharing_a_start: count(CENSUS_SHARED_STARTS)?,
    })
}
