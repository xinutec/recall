//! Results for the clips whose lines no stored transcription backs (#1916).
//! Render draws a clip from its model result, and these clips have none: the
//! job rows are gone. Each gets its transcription queued again (its
//! diarization follows on its own), and the speaker pass is told to leave the
//! clip's lines alone, so nothing shown changes before reads switch (#1917).
//! The turns pass already skips a clip with lines. Deleted with the old
//! machinery (#1919).

use crate::clips;
use crate::ledger::{Outcome, record};
use audiocore::instant::Stamp;
use audiocore::job::Kind;
use rusqlite::{Connection, OptionalExtension};
use std::path::Path;

crate::statements! {
    HAS_JOB: Ingest =
        "SELECT 1 FROM jobs WHERE kind = ?1 AND filename = ?2";
    QUEUE: Ingest =
        "INSERT INTO jobs (kind, filename, created_utc) VALUES (?1, ?2, ?3)";
}

/// What [`queue`] found, and with `apply`, did.
#[derive(Debug, Default, serde::Serialize)]
pub struct Backfill {
    /// Clips shown today with no stored transcription: queued.
    pub queued: usize,
    /// Of those, by source.
    pub by_source: std::collections::BTreeMap<String, usize>,
    /// Shown clips that have a transcription job but no answer (refused, or
    /// still running): left alone.
    pub job_without_answer: Vec<i64>,
    /// Shown clips whose file is missing: nothing to transcribe.
    pub missing_file: Vec<i64>,
    /// Audio rows with no clip.
    pub unmapped_audio_rows: usize,
    pub applied: bool,
}

/// Queue a transcription for every shown clip with no stored one. Without
/// `apply` nothing is written.
///
/// # Errors
/// If a plane refuses or a stored result cannot be read.
pub fn queue(
    meaning: &Connection,
    ingest: &Connection,
    root: &Path,
    apply: bool,
) -> Result<Backfill, Box<dyn std::error::Error>> {
    let mut done = Backfill {
        applied: apply,
        ..Backfill::default()
    };
    let mut todo = Vec::new();
    for path in crate::shadow::shown_paths(meaning)? {
        let Some(clip) = clips::for_audio_path(ingest, root, &path)? else {
            done.unmapped_audio_rows += 1;
            continue;
        };
        if crate::rendering::facts(ingest, &clip)?.heard.is_some() {
            continue;
        }
        let has_job = HAS_JOB
            .query_row(
                ingest,
                rusqlite::params![Kind::TranscribeSegment, clip.filename],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if has_job {
            done.job_without_answer.push(clip.id.stored());
        } else if !root.join(&clip.path).is_file() {
            done.missing_file.push(clip.id.stored());
        } else {
            *done.by_source.entry(clip.source.to_string()).or_default() += 1;
            todo.push(clip.filename);
        }
    }
    done.queued = todo.len();
    if apply {
        let when = chrono::Utc::now();
        let now = Stamp::of(when);
        // The queue's own spelling of a job's creation.
        let created = when.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let tx = crate::sql::write_shared(ingest)?;
        for filename in &todo {
            QUEUE.execute(
                &tx,
                rusqlite::params![Kind::TranscribeSegment, filename, created],
            )?;
            record(
                &tx,
                Kind::DiarizeSegment,
                filename,
                Outcome::LinesPredateResult,
                None,
                &now,
            )?;
        }
        tx.commit()?;
    }
    Ok(done)
}
