//! Uploads stored before the ingest plane existed, moved into it.
//!
//! The first uploads were stored at `<root>/<source>/` with no `segments` row,
//! so the queue never leases them and the blob route cannot serve them. [`run`]
//! moves each to `<root>/ingest/<source>/`, as an upload is stored today: the
//! file, its `segments` row, and the path in both planes.
//!
//! Run by hand (`recalld --root <root> move-old-uploads`), a dry run unless
//! `--apply`. The file is linked in before the rows move and unlinked after, so
//! every stored path stays playable, and an interrupted run is finished by the
//! next.

use crate::sql;
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::Connection;
use std::path::Path;

crate::statements! {
    OUTSIDE: Meaning =
        "SELECT a.source_id, a.path FROM audio_segments a
         JOIN sources s ON s.id = a.source_id
         WHERE s.kind = 'upload' AND a.path NOT LIKE ?1 || '/ingest/%'
         ORDER BY a.path";
    REPOINT_AUDIO: Meaning = "UPDATE audio_segments SET path = ?2 WHERE path = ?1";
    REPOINT_CLIP: Ingest = "UPDATE clips SET path = ?2 WHERE filename = ?1";
}

/// What a run did, or in a dry run would do.
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Moved {
    pub moved: Vec<String>,
    /// Files left where they are, each with the reason.
    pub skipped: Vec<(String, String)>,
}

/// Move every upload stored outside `ingest/`. Without `apply` nothing is
/// written.
///
/// # Errors
/// If a plane refuses. A file that cannot be moved is skipped, not an error.
pub fn run(
    root: &Path,
    meaning: &Connection,
    ingest: &Connection,
    now: DateTime<Utc>,
    apply: bool,
) -> rusqlite::Result<Moved> {
    let rows: Vec<(String, String)> = OUTSIDE
        .prepare(meaning)?
        .query_map([root.to_string_lossy()], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut done = Moved::default();
    for (source, path) in rows {
        match one(root, &source, &path, meaning, ingest, now, apply) {
            Ok(filename) => done.moved.push(filename),
            Err(Skip::Plane(err)) => return Err(err),
            Err(Skip::File(why)) => done.skipped.push((path, why)),
        }
    }
    Ok(done)
}

enum Skip {
    Plane(rusqlite::Error),
    File(String),
}

impl From<rusqlite::Error> for Skip {
    fn from(err: rusqlite::Error) -> Self {
        Self::Plane(err)
    }
}

/// One upload; returns its file name.
fn one(
    root: &Path,
    source: &str,
    path: &str,
    meaning: &Connection,
    ingest: &Connection,
    now: DateTime<Utc>,
    apply: bool,
) -> Result<String, Skip> {
    let file = |why: &str| Skip::File(why.to_owned());
    let old = Path::new(path);
    if old.parent() != Some(&root.join(source)) {
        return Err(file("not in <root>/<source>/"));
    }
    let filename = old
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| file("no file name"))?
        .to_owned();
    let started = audiocore::names::parse_segment_start(&filename)
        .ok_or_else(|| file("no start in the file name"))?;
    let dir = crate::store::source_dir(root, source);
    let new = dir.join(&filename);
    if !old.exists() && !new.exists() {
        return Err(file("the file is missing"));
    }
    if !apply {
        return Ok(filename);
    }
    if !new.exists() {
        let linked = std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::hard_link(old, &new))
            .and_then(|()| std::fs::File::open(&dir)?.sync_all());
        linked.map_err(|err| Skip::File(format!("cannot link: {err}")))?;
    }
    let bytes = std::fs::read(&new).map_err(|err| Skip::File(format!("cannot read: {err}")))?;
    let tx = sql::write_shared(ingest)?;
    if crate::store::lookup(&tx, &filename)?.is_none() {
        crate::store::insert(
            &tx,
            &crate::store::Row {
                source: source.to_owned(),
                filename: filename.clone(),
                start_utc: started.to_rfc3339_opts(SecondsFormat::Secs, true),
                bytes: bytes.len() as u64,
                sha256: crate::ingest::sha256_hex(&bytes),
                received_utc: now.to_rfc3339_opts(SecondsFormat::Secs, true),
                sent_utc: None,
            },
        )?;
    }
    REPOINT_CLIP.execute(&tx, (&filename, format!("ingest/{source}/{filename}")))?;
    tx.commit()?;
    REPOINT_AUDIO.execute(meaning, (path, new.to_string_lossy()))?;
    if old.exists() {
        std::fs::remove_file(old)
            .map_err(|err| Skip::File(format!("moved, but the old link stays: {err}")))?;
    }
    // Only an empty directory goes.
    if let Some(parent) = old.parent() {
        let _ = std::fs::remove_dir(parent);
    }
    Ok(filename)
}
