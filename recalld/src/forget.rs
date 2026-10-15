//! Forgetting a span of the household capture: the operator's deletion, run as
//! `recalld forget-span`, never a route (docs/architecture.md, principle 5).
//!
//! [`drop_recordings`] and [`crate::store::forget_files`] are what deleting a
//! recording means in each plane; a meeting's delete
//! ([`crate::sessions::delete_session`]) uses them too. Each recording is
//! tombstoned, so delivering it again is refused.

use audiocore::instant::{self, Stamp};
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::Connection;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

crate::statements! {
    UPLOADS: Meaning = "SELECT id FROM sources WHERE kind = 'upload'";
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

    /// The ingest plane's spelling: `Z`, whole seconds.
    fn ingest(&self) -> (String, String) {
        let spell = |t: DateTime<Utc>| t.to_rfc3339_opts(SecondsFormat::Secs, true);
        (spell(self.from), spell(self.to))
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
    pub sources: BTreeSet<String>,
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
    pub source: String,
    pub start_utc: String,
    pub path: String,
}

/// Remove recordings from the meaning plane, in the caller's transaction:
/// each tombstoned, its lines, its clips' rendering and edits, and its row.
/// `clips` and `live` (live line ids) are theirs.
pub(crate) fn drop_recordings(
    tx: &Connection,
    recordings: &[Recording],
    clips: &[i64],
    live: &[i64],
    now: &Stamp,
) -> rusqlite::Result<()> {
    for r in recordings {
        REMEMBER.execute(tx, (&r.source, &r.start_utc, now))?;
    }
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
    let uploads: Vec<String> = {
        let mut stmt = UPLOADS.prepare(meaning)?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let (from, to) = span.ingest();
    let mut segments: Vec<(String, String, String)> = {
        let mut stmt = SEGMENTS.prepare(ingest)?;
        let rows = stmt.query_map([&from, &to], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    segments.retain(|(source, ..)| !uploads.contains(source));
    let (from, to) = span.meaning();
    let audio: Vec<Recording> = {
        let mut stmt = AUDIO.prepare(meaning)?;
        let rows = stmt.query_map((&from, &to, json(&uploads)?), |r| {
            Ok(Recording {
                id: r.get(0)?,
                source: r.get(1)?,
                start_utc: r.get(2)?,
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
    let mut clips: Vec<i64> = Vec::new();
    for (_, filename, _) in &segments {
        if let Some(clip) = crate::clips::by_filename(ingest, filename)? {
            clips.push(clip.id.stored());
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
            .map(|s| s.0.clone())
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
    for (source, _, start) in &segments {
        if let Some(start) = Stamp::parse(start) {
            REMEMBER.execute(&tx, (source, &start, now))?;
        }
    }
    tx.commit()?;

    let filenames: Vec<String> = segments.iter().map(|s| s.1.clone()).collect();
    let tx = crate::sql::write(ingest)?;
    crate::store::forget_files(&tx, &filenames)?;
    tx.commit()?;

    // Files only after the commits, as a meeting's delete.
    let files: BTreeSet<PathBuf> = segments
        .iter()
        .map(|(source, filename, ..)| crate::store::source_dir(root, source).join(filename))
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
