//! Voiceprint enrolment: a person's naming of a stretch (a labelled fragment,
//! `labels::labelled`) becomes a reference vector, and a print whose naming
//! was taken back, renamed or kept from enrolment is forgotten. The corpus has
//! saturated, so this keeps up with new labels rather than improving
//! identification.

use crate::ledger::{Outcome, record};
use audiocore::instant::Stamp;
use audiocore::job::Kind;
use audiocore::work::Span;
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

crate::statements! {
    /// Every print and what it was learnt from: a naming act, or before the
    /// edit log a line or a correction.
    PRINT_SOURCES: Meaning =
        "SELECT source_edit_id, source_segment_id, source_correction_id FROM speaker_embeddings";
    /// The naming acts recovered from a line or a correction, with that origin.
    SEEDED_NAMINGS: Meaning =
        "SELECT id, origin FROM edits
         WHERE origin IS NOT NULL AND json_extract(act, '$.Speaker') IS NOT NULL";
    FORGET_ACT_PRINT: Meaning = "DELETE FROM speaker_embeddings WHERE source_edit_id = ?1";
    FORGET_LINE_PRINT: Meaning = "DELETE FROM speaker_embeddings WHERE source_segment_id = ?1";
    FORGET_CORRECTION_PRINT: Meaning =
        "DELETE FROM speaker_embeddings WHERE source_correction_id = ?1";
    UNDERIVED: Ingest =
        "SELECT s.filename FROM segments s
             WHERE NOT EXISTS (SELECT 1 FROM jobs j
                               WHERE j.kind = ?1 AND j.filename = s.filename)
             ORDER BY s.start_utc DESC";
    DERIVE_JOB: Ingest =
        "INSERT OR IGNORE INTO jobs (kind, filename, created_utc) VALUES (?1, ?2, ?3)";
    ADD_SPEAKER: Meaning =
        "INSERT OR IGNORE INTO speakers (name) VALUES (?1)";
    SPEAKER_ID: Meaning =
        "SELECT id FROM speakers WHERE name = ?1";
    ADD_EMBEDDING: Meaning =
        "INSERT INTO speaker_embeddings (speaker_id, vector, created_utc, source_edit_id)
         VALUES (?1, ?2, ?3, ?4)";
    FINISHED: Ingest =
        "SELECT j.filename, j.result FROM jobs j
             WHERE j.kind = ?1 AND j.done_utc IS NOT NULL AND j.result IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM pass_ledger l
                               WHERE l.kind = ?1 AND l.filename = j.filename)
             ORDER BY j.filename ASC";
}

/// A naming shorter than this enrols a useless print.
const MIN_SECONDS: f64 = 1.0;

/// The naming acts that already have a print: by the act, or for an act
/// recovered from the old tables, by the line or correction it came from.
fn printed(meaning: &Connection) -> rusqlite::Result<HashSet<i64>> {
    let mut by_act = HashSet::new();
    let mut by_line = HashSet::new();
    let mut by_correction = HashSet::new();
    let mut stmt = PRINT_SOURCES.prepare(meaning)?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, Option<i64>>(0)?,
            r.get::<_, Option<i64>>(1)?,
            r.get::<_, Option<i64>>(2)?,
        ))
    })?;
    for row in rows {
        let (act, line, correction) = row?;
        by_act.extend(act);
        by_line.extend(line);
        by_correction.extend(correction);
    }
    for (act, origin) in seeded(meaning)? {
        let found = match origin {
            Origin::Line(id) => by_line.contains(&id),
            Origin::Correction(id) => by_correction.contains(&id),
        };
        if found {
            by_act.insert(act);
        }
    }
    Ok(by_act)
}

/// Where a recovered naming act came from.
#[derive(Debug, Clone, Copy)]
enum Origin {
    Line(i64),
    Correction(i64),
}

/// The naming acts recovered from a line or a correction, by `edits.origin`'s
/// spelling (`Line(id)`, `Correction(id)`).
fn seeded(meaning: &Connection) -> rusqlite::Result<Vec<(i64, Origin)>> {
    let mut stmt = SEEDED_NAMINGS.prepare(meaning)?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = Vec::new();
    for row in rows {
        let (act, origin) = row?;
        let number = |prefix: &str| {
            origin
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_suffix(')'))
                .and_then(|n| n.parse::<i64>().ok())
        };
        if let Some(id) = number("Line(") {
            out.push((act, Origin::Line(id)));
        } else if let Some(id) = number("Correction(") {
            out.push((act, Origin::Correction(id)));
        }
    }
    Ok(out)
}

/// Forget every print whose naming no longer teaches it: taken back, named
/// again or kept from enrolment. Returns how many.
///
/// # Errors
/// If the meaning plane refuses or the edit log does not read.
pub fn forget_unwanted(meaning: &Connection) -> rusqlite::Result<usize> {
    let wanted: HashSet<i64> = crate::labels::labelled(meaning)?
        .iter()
        .map(|l| l.id.stored())
        .collect();
    let acts: Vec<i64> = {
        let mut stmt = PRINT_SOURCES.prepare(meaning)?;
        let rows = stmt.query_map([], |r| r.get::<_, Option<i64>>(0))?;
        rows.filter_map(Result::transpose)
            .collect::<rusqlite::Result<_>>()?
    };
    let mut forgotten = 0;
    for act in acts.into_iter().filter(|a| !wanted.contains(a)) {
        forgotten += FORGET_ACT_PRINT.execute(meaning, [act])?;
    }
    for (act, origin) in seeded(meaning)? {
        if wanted.contains(&act) {
            continue;
        }
        forgotten += match origin {
            Origin::Line(id) => FORGET_LINE_PRINT.execute(meaning, [id])?,
            Origin::Correction(id) => FORGET_CORRECTION_PRINT.execute(meaning, [id])?,
        };
    }
    Ok(forgotten)
}

/// Every labelled fragment awaiting a print, by its clip's ingest filename.
///
/// # Errors
/// If either plane refuses or the edit log does not read.
pub fn pending(meaning: &Connection, ingest: &Connection) -> rusqlite::Result<Vec<(String, Span)>> {
    let printed = printed(meaning)?;
    let clips: HashMap<transcript::ClipId, transcript::Clip> = crate::clips::all(ingest)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?
        .into_iter()
        .map(|c| (c.id, c))
        .collect();
    let mut out = Vec::new();
    for l in crate::labels::labelled(meaning)? {
        if printed.contains(&l.id.stored()) || (l.span.micros() as f64) < MIN_SECONDS * 1e6 {
            continue;
        }
        let Some(clip) = clips.get(&l.clip) else {
            continue;
        };
        let seconds = |at: transcript::Instant| (at.micros() - clip.start.micros()) as f64 / 1e6;
        out.push((
            clip.filename.clone(),
            Span {
                segment_id: l.id.stored(),
                // A negative seek makes ffmpeg return the whole clip.
                start_s: seconds(l.span.start()).max(0.0),
                end_s: seconds(l.span.end()),
            },
        ));
    }
    Ok(out)
}

/// The stretches to embed from one leased clip, by its ingest filename.
///
/// # Errors
/// If either plane refuses.
pub fn spans_for(
    meaning: &Connection,
    ingest: &Connection,
    filename: &str,
) -> rusqlite::Result<Vec<Span>> {
    Ok(pending(meaning, ingest)?
        .into_iter()
        .filter(|(f, _)| f == filename)
        .map(|(_, span)| span)
        .collect())
}

/// Fill an enrolment job's spans at lease time: they change whenever somebody
/// renames a voice. Other kinds never open the meaning plane.
///
/// # Errors
/// If the meaning plane refuses.
pub fn attach_spans(
    root: &std::path::Path,
    job: &mut audiocore::work::Job,
) -> rusqlite::Result<()> {
    if job.kind != Kind::EnrollSpeaker {
        return Ok(());
    }
    job.spans = spans_for(
        &crate::reads::open(root)?,
        &crate::store::open(root)?,
        &job.filename,
    )?;
    Ok(())
}

/// Derive one enrolment job per delivered clip with a naming awaiting a print,
/// up to `limit`; the spans travel with the lease.
///
/// # Errors
/// If either database refuses.
pub fn derive_jobs(
    ingest: &Connection,
    meaning: &Connection,
    now: DateTime<Utc>,
    limit: usize,
) -> rusqlite::Result<usize> {
    let wanted: HashSet<String> = pending(meaning, ingest)?
        .into_iter()
        .map(|(s, _)| s)
        .collect();
    if wanted.is_empty() {
        return Ok(0);
    }
    let candidates: Vec<String> = {
        let mut stmt = UNDERIVED.prepare(ingest)?;
        let rows = stmt.query_map([Kind::EnrollSpeaker], |r| r.get::<_, String>(0))?;
        rows.collect::<Result<_, _>>()?
    };
    let mut inserted = 0;
    for filename in candidates {
        if inserted >= limit {
            break;
        }
        if !wanted.contains(&filename) {
            continue;
        }
        inserted += DERIVE_JOB.execute(
            ingest,
            (
                Kind::EnrollSpeaker,
                &filename,
                audiocore::instant::IngestStamp::of(now),
            ),
        )?;
    }
    Ok(inserted)
}

// --- writing what the runner embedded ----------------------------------------

pub use audiocore::shim::voices::Print;
use audiocore::shim::{Stored, voices::Prints};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Enrolled {
    pub clips: usize,
    pub prints: usize,
    /// Spans whose naming was taken back, renamed or printed meanwhile, or
    /// that came back empty.
    pub stale: usize,
}

/// Whose voice each naming awaiting a print is now; one taken back, renamed
/// or printed meanwhile is absent.
fn still_wanted(meaning: &Connection) -> rusqlite::Result<HashMap<i64, String>> {
    let printed = printed(meaning)?;
    Ok(crate::labels::labelled(meaning)?
        .into_iter()
        .filter(|l| !printed.contains(&l.id.stored()))
        .map(|l| (l.id.stored(), l.name.as_str().to_owned()))
        .collect())
}

fn enrol_one(
    meaning: &Connection,
    person: &str,
    print: &Print,
    now: &Stamp,
) -> rusqlite::Result<()> {
    // An error, never a stand-in: an empty print matches everyone.
    let vector = serde_json::to_string(&print.vector)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
    ADD_SPEAKER.execute(meaning, [person])?;
    let speaker_id: i64 = SPEAKER_ID.query_row(meaning, [person], |r| r.get(0))?;
    ADD_EMBEDDING.execute(
        meaning,
        rusqlite::params![speaker_id, vector, now, print.segment_id],
    )?;
    Ok(())
}

/// Turn finished enrolment results into voiceprints. Every clip examined is
/// ledgered, even one that enrols nothing.
///
/// # Errors
/// If either database refuses.
pub fn write_pass(
    meaning: &Connection,
    ingest: &Connection,
    now: &Stamp,
    limit: usize,
) -> rusqlite::Result<Enrolled> {
    let candidates: Vec<(String, String)> = {
        let mut stmt = FINISHED.prepare(ingest)?;
        let rows = stmt.query_map([Kind::EnrollSpeaker], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<Result<_, _>>()?
    };

    let mut pass = Enrolled::default();
    let mut wanted = still_wanted(meaning)?;
    for (filename, result) in candidates {
        if pass.clips >= limit {
            break;
        }
        pass.clips += 1;
        let Ok(reply) = Stored::<Prints>::parse(&result) else {
            record(
                ingest,
                Kind::EnrollSpeaker,
                &filename,
                Outcome::Unreadable,
                now,
            )?;
            continue;
        };
        let Some(body) = reply.result.filter(|_| reply.ok) else {
            record(
                ingest,
                Kind::EnrollSpeaker,
                &filename,
                Outcome::Refused,
                now,
            )?;
            continue;
        };
        let mut wrote = 0;
        for print in &body.prints {
            // At cosine 0 against everyone, an empty vector would become
            // somebody's best match on quiet audio.
            if print.vector.is_empty() {
                pass.stale += 1;
                continue;
            }
            match wanted.remove(&print.segment_id) {
                Some(person) => {
                    enrol_one(meaning, &person, print, now)?;
                    wrote += 1;
                }
                None => pass.stale += 1,
            }
        }
        pass.prints += wrote;
        let outcome = if wrote > 0 {
            Outcome::Enrolled
        } else {
            Outcome::NothingEnrolled
        };
        record(ingest, Kind::EnrollSpeaker, &filename, outcome, now)?;
    }
    Ok(pass)
}
