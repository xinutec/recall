//! Forgetting a span of the household capture: the operator's deletion, run as
//! `recalld forget-span`, never a route (docs/architecture.md, principle 5).
//!
//! [`drop_recordings`] and [`crate::store::forget_files`] are what deleting a
//! recording means in each plane; a meeting's delete
//! ([`crate::sessions::delete_session`]) uses them too. Each recording is
//! tombstoned, so delivering it again is refused.

use audiocore::instant::IngestStamp;
use audiocore::instant::{self, Stamp};
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use transcript::{ClipId, SourceId};

crate::statements! {
    UPLOADS: Meaning = "SELECT id FROM sources WHERE kind = ?1";
    /// `?1`, `?2`: the span in the ingest plane's spelling.
    SEGMENTS: Ingest =
        "SELECT source, filename, start_utc FROM segments WHERE start_utc >= ?1 AND start_utc < ?2";
    /// `?3`: the uploads' source ids as JSON.
    AUDIO: Meaning =
        "SELECT id, source_id, start_utc, path FROM audio_segments
         WHERE start_utc >= ?1 AND start_utc < ?2
           AND source_id NOT IN (SELECT value FROM json_each(?3))";
    LIVE: Meaning = "SELECT id FROM live_lines WHERE start_utc >= ?1 AND start_utc < ?2";
    /// `?1`: audio row ids, `?2`: clip ids, as JSON.
    VOICEPRINTS: Meaning =
        "SELECT count(*) FROM speaker_embeddings
         WHERE source_segment_id IN (SELECT value FROM json_each(?1))
            OR source_edit_id IN
               (SELECT id FROM edits WHERE clip_id IN (SELECT value FROM json_each(?2)))";
    /// `?1`: clip ids, `?2`: live line ids, as JSON.
    COUNT_LINES: Meaning =
        "SELECT count(*) FROM rendered_lines
         WHERE clip_id IN (SELECT value FROM json_each(?1))
            OR live_id IN (SELECT value FROM json_each(?2))";
    COUNT_EDITS: Meaning =
        "SELECT count(*) FROM edits WHERE clip_id IN (SELECT value FROM json_each(?1))";
    REMEMBER: Meaning =
        "INSERT OR IGNORE INTO deleted_segments (source_id, start_utc, deleted_utc)
         VALUES (?1, ?2, ?3)";
    /// `?1`: clip ids, `?2`: audio row ids, `?3`: live line ids, as JSON.
    DROP_LINES: Meaning =
        "DELETE FROM rendered_lines
         WHERE clip_id IN (SELECT value FROM json_each(?1))
            OR audio_segment_id IN (SELECT value FROM json_each(?2))
            OR live_id IN (SELECT value FROM json_each(?3))";
    DROP_RENDERED: Meaning =
        "DELETE FROM rendered_clips WHERE clip_id IN (SELECT value FROM json_each(?1))";
    DROP_EDITS: Meaning =
        "DELETE FROM edits WHERE clip_id IN (SELECT value FROM json_each(?1))";
    DROP_TRANSCRIBED_BEFORE_JOBS: Meaning =
        "DELETE FROM transcribed_before_jobs WHERE audio_segment_id IN (SELECT value FROM json_each(?1))";
    DROP_LIVE: Meaning = "DELETE FROM live_lines WHERE id IN (SELECT value FROM json_each(?1))";
    DROP_AUDIO: Meaning =
        "DELETE FROM audio_segments WHERE id IN (SELECT value FROM json_each(?1))";
    SOURCE_KIND: Meaning = "SELECT kind FROM sources WHERE id = ?1";
    SOURCE_SEGMENTS: Ingest = "SELECT source, filename, start_utc FROM segments WHERE source = ?1";
    SOURCE_AUDIO: Meaning =
        "SELECT id, source_id, start_utc, path FROM audio_segments WHERE source_id = ?1";
    DROP_TOMBSTONES: Meaning = "DELETE FROM deleted_segments WHERE source_id = ?1";
    DROP_SOURCE: Meaning = "DELETE FROM sources WHERE id = ?1";
    SOURCE_ROW: Meaning = "SELECT name, kind FROM sources WHERE id = ?1";
    SEGMENTS_OF: Ingest = "SELECT filename FROM segments WHERE source = ?1";
    COUNT_SOURCE_LINES: Meaning = "SELECT count(*) FROM rendered_lines WHERE source_id = ?1";
    /// The rename, one statement per table. `?1`: the old id, `?2`: the new,
    /// `?3`: the old filename prefix (`<id>-`), `?4`: the new. A filename
    /// without its source's prefix keeps its name and only changes source.
    /// The filename-keyed tables go first: they find their rows through
    /// `segments`, which is renamed last.
    RENAME_SPEECH_FILES: Ingest =
        "UPDATE segment_speech SET filename = ?4 || substr(filename, length(?3) + 1)
         WHERE source = ?1 AND substr(filename, 1, length(?3)) = ?3";
    RENAME_LEVELS_FILES: Ingest =
        "UPDATE segment_levels SET filename = ?4 || substr(filename, length(?3) + 1)
         WHERE source = ?1 AND substr(filename, 1, length(?3)) = ?3";
    RENAME_JOBS_FILES: Ingest =
        "UPDATE jobs SET filename = ?4 || substr(filename, length(?3) + 1)
         WHERE filename IN (SELECT filename FROM segments WHERE source = ?1)
           AND substr(filename, 1, length(?3)) = ?3";
    RENAME_LEDGER_FILES: Ingest =
        "UPDATE pass_ledger SET filename = ?4 || substr(filename, length(?3) + 1)
         WHERE filename IN (SELECT filename FROM segments WHERE source = ?1)
           AND substr(filename, 1, length(?3)) = ?3";
    RESOURCE_SPEECH: Ingest = "UPDATE segment_speech SET source = ?2 WHERE source = ?1";
    RESOURCE_LEVELS: Ingest = "UPDATE segment_levels SET source = ?2 WHERE source = ?1";
    RENAME_SEGMENTS: Ingest =
        "UPDATE segments SET source = ?2, filename = ?4 || substr(filename, length(?3) + 1)
         WHERE source = ?1 AND substr(filename, 1, length(?3)) = ?3";
    RESOURCE_SEGMENTS: Ingest = "UPDATE segments SET source = ?2 WHERE source = ?1";
    /// `?5`: the old directory with its trailing slash, `?6`: the new. A path
    /// is absolute here, as `audio_segments` records it.
    RENAME_AUDIO: Meaning =
        "UPDATE audio_segments
         SET source_id = ?2,
             path = ?6 || ?4 || substr(path, length(?5) + length(?3) + 1)
         WHERE source_id = ?1 AND substr(path, 1, length(?5) + length(?3)) = ?5 || ?3";
    RESOURCE_AUDIO: Meaning =
        "UPDATE audio_segments
         SET source_id = ?2, path = ?6 || substr(path, length(?5) + 1)
         WHERE source_id = ?1 AND substr(path, 1, length(?5)) = ?5";
    RENAME_EVENTS: Meaning = "UPDATE capture_events SET source_id = ?2 WHERE source_id = ?1";
    RENAME_TOMBSTONES: Meaning = "UPDATE deleted_segments SET source_id = ?2 WHERE source_id = ?1";
    RENAME_EDITS: Meaning = "UPDATE edits SET source_id = ?2 WHERE source_id = ?1";
    RENAME_LINES: Meaning = "UPDATE rendered_lines SET source_id = ?2 WHERE source_id = ?1";
    INSERT_SOURCE: Meaning = "INSERT INTO sources (id, name, kind) VALUES (?1, ?2, ?3)";
}

/// `[from, to)`, in UTC.
#[derive(Debug, Clone, Copy)]
pub struct Span {
    from: DateTime<Utc>,
    to: DateTime<Utc>,
}

impl Span {
    /// `None` unless both are instants and `from` is before `to`.
    #[must_use]
    pub fn parse(from: &str, to: &str) -> Option<Self> {
        let (from, to) = (instant::parse_utc(from)?, instant::parse_utc(to)?);
        (from < to).then_some(Self { from, to })
    }

    fn ingest(&self) -> (IngestStamp, IngestStamp) {
        (IngestStamp::of(self.from), IngestStamp::of(self.to))
    }

    fn meaning(&self) -> (Stamp, Stamp) {
        (Stamp::of(self.from), Stamp::of(self.to))
    }
}

/// What a span held, and whether it is gone.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Forgotten {
    pub applied: bool,
    /// Stored files with their ingest rows.
    pub segments: usize,
    pub audio_rows: usize,
    pub lines: usize,
    pub live_lines: usize,
    pub edits: usize,
    pub sources: BTreeSet<SourceId>,
    /// Files that would not unlink, rows already gone; empty when all did.
    pub left_on_disk: Vec<PathBuf>,
}

#[derive(Debug)]
pub enum ForgetError {
    Db(rusqlite::Error),
    Clip(crate::clips::ClipError),
    /// A voiceprint was learnt from audio in the span: forgetting it would
    /// leave the voiceprint without its source.
    Voiceprint,
    /// Only a derived source can be retired: anything else is a recording.
    NotDerived(SourceId),
    /// A rename needs a source that has a row and a new id that has none.
    NoSuchSource(SourceId),
    SourceExists(SourceId),
    /// A rename's file move failed; the rows were not changed.
    Move(PathBuf, std::io::Error),
}

impl From<rusqlite::Error> for ForgetError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Db(err)
    }
}

impl From<crate::clips::ClipError> for ForgetError {
    fn from(err: crate::clips::ClipError) -> Self {
        Self::Clip(err)
    }
}

impl std::fmt::Display for ForgetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db(err) => write!(f, "{err}"),
            Self::Clip(err) => write!(f, "{err}"),
            Self::Voiceprint => f.write_str("a voiceprint was learnt from audio in the span"),
            Self::NotDerived(source) => write!(f, "{source} is not a derived source: refused"),
            Self::NoSuchSource(source) => write!(f, "{source} has no sources row"),
            Self::SourceExists(source) => write!(f, "{source} already exists"),
            Self::Move(path, err) => write!(f, "{}: {err}", path.display()),
        }
    }
}

fn json<T: Serialize>(values: &[T]) -> rusqlite::Result<String> {
    serde_json::to_string(values).map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))
}

fn count(r: &rusqlite::Row<'_>) -> rusqlite::Result<usize> {
    let n: i64 = r.get(0)?;
    usize::try_from(n).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, n))
}

/// One meaning-plane audio row.
pub(crate) struct Recording {
    pub id: i64,
    pub source: SourceId,
    pub start: Stamp,
    pub path: String,
}

/// One ingest-plane file, its start respelled for the meaning plane.
struct Segment {
    source: SourceId,
    filename: String,
    start: Stamp,
}

/// Remove recordings from the meaning plane, in the caller's transaction:
/// each tombstoned, its lines, its clips' rendering and edits, and its row.
/// `clips` and `live` (live line ids) are theirs.
pub(crate) fn drop_recordings(
    tx: &Connection,
    recordings: &[Recording],
    clips: &[ClipId],
    live: &[i64],
    now: &Stamp,
) -> rusqlite::Result<()> {
    for r in recordings {
        REMEMBER.execute(tx, (r.source.as_str(), &r.start, now))?;
    }
    drop_rows(tx, recordings, clips, live)
}

/// [`drop_recordings`] without the tombstones.
fn drop_rows(
    tx: &Connection,
    recordings: &[Recording],
    clips: &[ClipId],
    live: &[i64],
) -> rusqlite::Result<()> {
    let ids: Vec<i64> = recordings.iter().map(|r| r.id).collect();
    let (clips, ids, live) = (json(clips)?, json(&ids)?, json(live)?);
    DROP_LINES.execute(tx, (&clips, &ids, &live))?;
    DROP_RENDERED.execute(tx, [&clips])?;
    DROP_EDITS.execute(tx, [&clips])?;
    DROP_TRANSCRIBED_BEFORE_JOBS.execute(tx, [&ids])?;
    DROP_LIVE.execute(tx, [&live])?;
    DROP_AUDIO.execute(tx, [&ids])?;
    Ok(())
}

/// Forget the household capture in `span`; with `apply` false, only say what
/// it holds. The meaning plane commits first, then the ingest plane, then the
/// files go: a failure part-way is finished by running it again.
pub fn span(
    meaning: &mut Connection,
    ingest: &mut Connection,
    root: &Path,
    span: &Span,
    now: &Stamp,
    apply: bool,
) -> Result<Forgotten, ForgetError> {
    let uploads: Vec<SourceId> = {
        let mut stmt = UPLOADS.prepare(meaning)?;
        let rows = stmt.query_map([crate::sources::SourceKind::Upload], |r| {
            crate::sql::source_id(r, 0)
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let (from, to) = span.ingest();
    let mut segments: Vec<Segment> = {
        let mut stmt = SEGMENTS.prepare(ingest)?;
        let rows = stmt.query_map((&from, &to), |r| {
            Ok(Segment {
                source: crate::sql::source_id(r, 0)?,
                filename: r.get(1)?,
                start: r.get(2)?,
            })
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    segments.retain(|s| !uploads.contains(&s.source));
    let (from, to) = span.meaning();
    let audio: Vec<Recording> = {
        let mut stmt = AUDIO.prepare(meaning)?;
        let rows = stmt.query_map((&from, &to, json(&uploads)?), |r| {
            Ok(Recording {
                id: r.get(0)?,
                source: crate::sql::source_id(r, 1)?,
                start: r.get(2)?,
                path: r.get(3)?,
            })
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let live: Vec<i64> = {
        let mut stmt = LIVE.prepare(meaning)?;
        let rows = stmt.query_map((&from, &to), |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut clips: Vec<ClipId> = Vec::new();
    for segment in &segments {
        if let Some(clip) = crate::clips::by_filename(ingest, &segment.filename)? {
            clips.push(clip.id);
        }
    }
    let audio_ids: Vec<i64> = audio.iter().map(|a| a.id).collect();
    let (clips_json, audio_json, live_json) = (json(&clips)?, json(&audio_ids)?, json(&live)?);

    let voiceprints: i64 =
        VOICEPRINTS.query_row(meaning, (&audio_json, &clips_json), |r| r.get(0))?;
    if voiceprints > 0 {
        return Err(ForgetError::Voiceprint);
    }
    let lines = COUNT_LINES.query_row(meaning, (&clips_json, &live_json), count)?;
    let edits = COUNT_EDITS.query_row(meaning, [&clips_json], count)?;
    let mut forgotten = Forgotten {
        applied: apply,
        segments: segments.len(),
        audio_rows: audio.len(),
        lines,
        live_lines: live.len(),
        edits,
        sources: segments
            .iter()
            .map(|s| s.source.clone())
            .chain(audio.iter().map(|a| a.source.clone()))
            .collect(),
        left_on_disk: Vec::new(),
    };
    if !apply {
        return Ok(forgotten);
    }

    let tx = crate::sql::write(meaning)?;
    drop_recordings(&tx, &audio, &clips, &live, now)?;
    // A minute not yet in the meaning plane is tombstoned too.
    for segment in &segments {
        REMEMBER.execute(&tx, (segment.source.as_str(), &segment.start, now))?;
    }
    tx.commit()?;

    let filenames: Vec<String> = segments.iter().map(|s| s.filename.clone()).collect();
    let tx = crate::sql::write(ingest)?;
    crate::store::forget_files(&tx, &filenames)?;
    tx.commit()?;

    // Files only after the commits, as a meeting's delete.
    let files: BTreeSet<PathBuf> = segments
        .iter()
        .map(|s| crate::store::source_dir(root, s.source.as_str()).join(&s.filename))
        .chain(audio.iter().map(|a| PathBuf::from(&a.path)))
        .collect();
    for file in files {
        if let Err(err) = std::fs::remove_file(&file)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            forgotten.left_on_disk.push(file);
        }
    }
    Ok(forgotten)
}

/// Retire a derived source: every row it has in both planes, its files, its
/// tombstones and its `sources` row. A stream built from the microphones is
/// not the record (they are), so nothing is tombstoned, and a source that was
/// recorded is refused. With `apply` false, only say what it holds.
pub fn retire_source(
    meaning: &mut Connection,
    ingest: &mut Connection,
    root: &Path,
    source: &SourceId,
    apply: bool,
) -> Result<Forgotten, ForgetError> {
    let kind: Option<crate::sources::SourceKind> = {
        use rusqlite::OptionalExtension as _;
        SOURCE_KIND
            .query_row(meaning, [source.as_str()], |r| r.get(0))
            .optional()?
    };
    if kind != Some(crate::sources::SourceKind::Derived) {
        return Err(ForgetError::NotDerived(source.clone()));
    }
    let segments: Vec<Segment> = {
        let mut stmt = SOURCE_SEGMENTS.prepare(ingest)?;
        let rows = stmt.query_map([source.as_str()], |r| {
            Ok(Segment {
                source: crate::sql::source_id(r, 0)?,
                filename: r.get(1)?,
                start: r.get(2)?,
            })
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let audio: Vec<Recording> = {
        let mut stmt = SOURCE_AUDIO.prepare(meaning)?;
        let rows = stmt.query_map([source.as_str()], |r| {
            Ok(Recording {
                id: r.get(0)?,
                source: crate::sql::source_id(r, 1)?,
                start: r.get(2)?,
                path: r.get(3)?,
            })
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut clips: Vec<ClipId> = Vec::new();
    for segment in &segments {
        if let Some(clip) = crate::clips::by_filename(ingest, &segment.filename)? {
            clips.push(clip.id);
        }
    }
    let audio_ids: Vec<i64> = audio.iter().map(|a| a.id).collect();
    let (clips_json, audio_json) = (json(&clips)?, json(&audio_ids)?);
    let voiceprints: i64 =
        VOICEPRINTS.query_row(meaning, (&audio_json, &clips_json), |r| r.get(0))?;
    if voiceprints > 0 {
        return Err(ForgetError::Voiceprint);
    }
    let mut forgotten = Forgotten {
        applied: apply,
        segments: segments.len(),
        audio_rows: audio.len(),
        lines: COUNT_LINES.query_row(meaning, (&clips_json, "[]"), count)?,
        live_lines: 0,
        edits: COUNT_EDITS.query_row(meaning, [&clips_json], count)?,
        sources: std::iter::once(source.clone()).collect(),
        left_on_disk: Vec::new(),
    };
    if !apply {
        return Ok(forgotten);
    }

    let tx = crate::sql::write(meaning)?;
    drop_rows(&tx, &audio, &clips, &[])?;
    DROP_TOMBSTONES.execute(&tx, [source.as_str()])?;
    DROP_SOURCE.execute(&tx, [source.as_str()])?;
    tx.commit()?;

    let filenames: Vec<String> = segments.iter().map(|s| s.filename.clone()).collect();
    let tx = crate::sql::write(ingest)?;
    crate::store::forget_files(&tx, &filenames)?;
    tx.commit()?;

    let files: BTreeSet<PathBuf> = segments
        .iter()
        .map(|s| crate::store::source_dir(root, s.source.as_str()).join(&s.filename))
        .chain(audio.iter().map(|a| PathBuf::from(&a.path)))
        .collect();
    for file in files {
        if let Err(err) = std::fs::remove_file(&file)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            forgotten.left_on_disk.push(file);
        }
    }
    Ok(forgotten)
}

/// What a rename moved, or would.
#[derive(Debug, Serialize)]
pub struct Renamed {
    pub applied: bool,
    pub from: SourceId,
    pub to: SourceId,
    /// Stored files with their ingest rows.
    pub segments: usize,
    pub audio_rows: usize,
    pub lines: usize,
    /// Files under the source's directory that no row names; moved with it.
    pub loose_files: usize,
    /// Its recorder's heartbeat and outbox rows (`crate::devices`).
    pub device_status: usize,
}

/// The disk half of a rename: every file into the new directory under its new
/// name, then the marker directory. A move that fails stops here, rows untouched.
fn move_source_dirs(
    root: &Path,
    from: &SourceId,
    to: &SourceId,
    on_disk: &[String],
    renamed: &dyn Fn(&str) -> String,
) -> Result<(), ForgetError> {
    let old_dir = crate::store::source_dir(root, from.as_str());
    let new_dir = crate::store::source_dir(root, to.as_str());
    if old_dir.exists() {
        std::fs::create_dir_all(&new_dir).map_err(|e| ForgetError::Move(new_dir.clone(), e))?;
        for file in on_disk {
            let (src, dst) = (old_dir.join(file), new_dir.join(renamed(file)));
            std::fs::rename(&src, &dst).map_err(|e| ForgetError::Move(src, e))?;
        }
        std::fs::remove_dir(&old_dir).map_err(|e| ForgetError::Move(old_dir.clone(), e))?;
    }
    let (old_marks, new_marks) = (root.join(from.as_str()), root.join(to.as_str()));
    if old_marks.is_dir() && !new_marks.exists() {
        std::fs::rename(&old_marks, &new_marks).map_err(|e| ForgetError::Move(old_marks, e))?;
    }
    Ok(())
}

/// The rows half of a rename, one transaction per plane: the ingest plane's
/// filename-keyed tables, clips and segments; then the meaning plane's new
/// `sources` row, its audio paths and every table naming the source, the old
/// row dropped last so the foreign keys hold throughout.
fn rename_rows(
    meaning: &mut Connection,
    ingest: &mut Connection,
    from: &SourceId,
    to: &SourceId,
    (old_prefix, new_prefix): (&str, &str),
    (name, kind): (&str, &str),
    root: &Path,
) -> Result<(), ForgetError> {
    let tx = crate::sql::write(ingest)?;
    // `segment_speech` and `segment_levels` reference `segments` by filename:
    // under immediate checking no order of renaming passes, so the keys are
    // checked once, at commit, when every side has moved.
    tx.pragma_update(None, "defer_foreign_keys", true)?;
    let args = (from.as_str(), to.as_str(), old_prefix, new_prefix);
    for statement in [
        RENAME_SPEECH_FILES,
        RENAME_LEVELS_FILES,
        RENAME_JOBS_FILES,
        RENAME_LEDGER_FILES,
    ] {
        statement.execute(&tx, args)?;
    }
    RESOURCE_SPEECH.execute(&tx, (from.as_str(), to.as_str()))?;
    RESOURCE_LEVELS.execute(&tx, (from.as_str(), to.as_str()))?;
    crate::clips::rename_source(&tx, from.as_str(), to.as_str(), old_prefix, new_prefix)?;
    RENAME_SEGMENTS.execute(&tx, args)?;
    RESOURCE_SEGMENTS.execute(&tx, (from.as_str(), to.as_str()))?;
    tx.commit()?;

    let old_abs = format!(
        "{}/",
        crate::store::source_dir(root, from.as_str()).display()
    );
    let new_abs = format!("{}/", crate::store::source_dir(root, to.as_str()).display());
    let tx = crate::sql::write(meaning)?;
    tx.pragma_update(None, "defer_foreign_keys", true)?;
    INSERT_SOURCE.execute(&tx, (to.as_str(), name, kind))?;
    let paths = (
        from.as_str(),
        to.as_str(),
        old_prefix,
        new_prefix,
        &old_abs,
        &new_abs,
    );
    RENAME_AUDIO.execute(&tx, paths)?;
    RESOURCE_AUDIO.execute(&tx, paths)?;
    for statement in [RENAME_EVENTS, RENAME_TOMBSTONES, RENAME_EDITS, RENAME_LINES] {
        statement.execute(&tx, (from.as_str(), to.as_str()))?;
    }
    DROP_SOURCE.execute(&tx, [from.as_str()])?;
    for key in [crate::devices::BEATS, crate::devices::REPORTS] {
        crate::devices::rename(&tx, key, from.as_str(), to.as_str())?;
    }
    tx.commit()?;
    Ok(())
}

/// Give `from` the id `to` everywhere: its `sources` row, every row naming it
/// in either plane, its files' names (`<id>-<stamp>.<ext>`, kept as the key:
/// a clip's id survives), its directory and its marker directory. The disk
/// moves first, inside a transaction on each plane, so a failed move leaves
/// the rows as they were. The recorder is the one thing not renamed: set its
/// id before it next delivers, or it starts a new source under the old name.
pub fn rename_source(
    meaning: &mut Connection,
    ingest: &mut Connection,
    root: &Path,
    from: &SourceId,
    to: &SourceId,
    apply: bool,
) -> Result<Renamed, ForgetError> {
    use rusqlite::OptionalExtension as _;
    let row: Option<(String, String)> = SOURCE_ROW
        .query_row(meaning, [from.as_str()], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let Some((name, kind)) = row else {
        return Err(ForgetError::NoSuchSource(from.clone()));
    };
    let taken: Option<String> = SOURCE_ROW
        .query_row(meaning, [to.as_str()], |r| r.get(0))
        .optional()?;
    if taken.is_some() || crate::store::source_dir(root, to.as_str()).exists() {
        return Err(ForgetError::SourceExists(to.clone()));
    }
    let filenames: Vec<String> = {
        let mut stmt = SEGMENTS_OF.prepare(ingest)?;
        let rows = stmt.query_map([from.as_str()], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let audio: Vec<Recording> = {
        let mut stmt = SOURCE_AUDIO.prepare(meaning)?;
        let rows = stmt.query_map([from.as_str()], |r| {
            Ok(Recording {
                id: r.get(0)?,
                source: crate::sql::source_id(r, 1)?,
                start: r.get(2)?,
                path: r.get(3)?,
            })
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let lines: usize = {
        let n: i64 = COUNT_SOURCE_LINES.query_row(meaning, [from.as_str()], |r| r.get(0))?;
        usize::try_from(n).unwrap_or(0)
    };
    let old_dir = crate::store::source_dir(root, from.as_str());
    let old_prefix = format!("{}-", from.as_str());
    let new_prefix = format!("{}-", to.as_str());
    let renamed = |file: &str| -> String {
        match file.strip_prefix(&old_prefix) {
            Some(rest) => format!("{new_prefix}{rest}"),
            None => file.to_owned(),
        }
    };
    let on_disk: Vec<String> = match std::fs::read_dir(&old_dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter_map(|e| e.file_name().into_string().ok())
            .collect(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(err) => return Err(ForgetError::Move(old_dir.clone(), err)),
    };
    let named: BTreeSet<&str> = filenames.iter().map(String::as_str).collect();
    let loose_files = on_disk
        .iter()
        .filter(|f| !named.contains(f.as_str()))
        .count();
    let mut report = Renamed {
        applied: apply,
        from: from.clone(),
        to: to.clone(),
        segments: filenames.len(),
        audio_rows: audio.len(),
        lines,
        loose_files,
        device_status: crate::devices::status_rows(meaning, from.as_str())?,
    };
    if !apply {
        return Ok(report);
    }

    move_source_dirs(root, from, to, &on_disk, &renamed)?;
    if let Err(err) = rename_rows(
        meaning,
        ingest,
        from,
        to,
        (&old_prefix, &new_prefix),
        (&name, &kind),
        root,
    ) {
        // The rows did not take: put the files back under their old names, so
        // the archive is as it was and the error is the only trace.
        let moved: Vec<String> = on_disk.iter().map(|f| renamed(f)).collect();
        let back = |file: &str| -> String {
            match file.strip_prefix(&new_prefix) {
                Some(rest) => format!("{old_prefix}{rest}"),
                None => file.to_owned(),
            }
        };
        move_source_dirs(root, to, from, &moved, &back)?;
        return Err(err);
    }
    report.applied = true;
    Ok(report)
}
