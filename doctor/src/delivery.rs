//! Store-and-forward delivery: is the fleet's copy keeping up, and did anything
//! collide? (docs/architecture.md)
//!
//! Compares every segment file on disk against audiod's uploader state
//! (`upload-state.sqlite`). A backlog is graded by the age of its oldest member,
//! since deliveries run oldest-first. A 409 conflict warns without failing:
//! nothing was lost, but Isis holds different bytes under the same name and a
//! person has to look.

use crate::capture::minutes;
use crate::check::{Check, Verdict, check};
use chrono::{DateTime, Duration, Utc};
use std::collections::BTreeSet;
use std::path::Path;

const OPEN_GRACE_MINUTES: i64 = 3;
const WARN_MINUTES: i64 = 30;
// A segment must not age out of the grace straight into a warning.
const _: () = assert!(OPEN_GRACE_MINUTES < WARN_MINUTES);
const FAIL_HOURS: i64 = 6;
const CONFLICTS_NAMED: usize = 3;

/// What the uploader has already handled, and what collided.
fn uploader_state(state: &Path) -> rusqlite::Result<(BTreeSet<String>, Vec<String>)> {
    let conn = rusqlite::Connection::open_with_flags(
        state,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?;
    let mut handled: BTreeSet<String> = conn
        .prepare("SELECT filename FROM uploads")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    let conflicts: Vec<String> = conn
        .prepare("SELECT filename FROM conflicts ORDER BY filename")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    handled.extend(conflicts.iter().cloned());
    Ok((handled, conflicts))
}

/// Every closed segment on disk that the uploader has not handled, by mtime.
///
/// The newest file in each source directory is skipped while it is inside the
/// open grace: that is the segment ffmpeg may still be writing, and it is not
/// undelivered, it is unfinished.
fn undelivered(out: &Path, handled: &BTreeSet<String>, now: DateTime<Utc>) -> Vec<DateTime<Utc>> {
    let grace = Duration::minutes(OPEN_GRACE_MINUTES);
    let mut backlog = Vec::new();
    let Ok(entries) = std::fs::read_dir(out) else {
        return backlog;
    };
    let mut source_dirs: Vec<_> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    source_dirs.sort();

    for dir in source_dirs {
        let source = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let Ok(files) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut names: Vec<String> = files
            .flatten()
            .filter(|e| e.path().is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| audiocore::names::parse(&source, name).is_ok_and(|n| n.ext.recorded()))
            .collect();
        if names.is_empty() {
            continue;
        }
        // The name embeds the UTC start, so sorting by name is chronological.
        names.sort();
        let newest = dir.join(names.last().expect("names is non-empty"));
        if let Ok(mtime) = newest.metadata().and_then(|m| m.modified())
            && now - DateTime::<Utc>::from(mtime) < grace
        {
            names.pop();
        }
        for name in names {
            if handled.contains(&name) {
                continue;
            }
            if let Ok(mtime) = dir.join(&name).metadata().and_then(|m| m.modified()) {
                backlog.push(DateTime::<Utc>::from(mtime));
            }
        }
    }
    backlog
}

/// Quiet when the uploader has never run here (no state file): a stock
/// deployment without stage B has no mirror to be behind on.
pub fn delivery_checks(out: &Path, now: DateTime<Utc>) -> Vec<Check> {
    let state = out.join("upload-state.sqlite");
    if !state.exists() {
        return Vec::new();
    }
    let Ok((handled, conflicts)) = uploader_state(&state) else {
        return Vec::new();
    };
    let backlog = undelivered(out, &handled, now);

    let oldest_age = backlog
        .iter()
        .map(|mtime| now - *mtime)
        .max()
        .unwrap_or_else(Duration::zero);
    let verdict = if oldest_age >= Duration::hours(FAIL_HOURS) {
        Verdict::Fail
    } else if oldest_age >= Duration::minutes(WARN_MINUTES) {
        Verdict::Warn
    } else {
        Verdict::Pass
    };

    let named = conflicts
        .iter()
        .take(CONFLICTS_NAMED)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ")
        + if conflicts.len() > CONFLICTS_NAMED {
            "…"
        } else {
            ""
        };

    vec![
        check(
            "sync",
            "delivery complete",
            verdict,
            if backlog.is_empty() {
                "every closed segment delivered and verified".to_owned()
            } else {
                format!(
                    "{} closed segment(s) undelivered, oldest {:.0}m",
                    backlog.len(),
                    minutes(oldest_age)
                )
            },
            format!("oldest undelivered < {WARN_MINUTES}m"),
        )
        .trend(backlog.len() as f64, "segments")
        .build(),
        check(
            "sync",
            "no delivery conflicts",
            if conflicts.is_empty() {
                Verdict::Pass
            } else {
                Verdict::Warn
            },
            if conflicts.is_empty() {
                "no name held by different bytes".to_owned()
            } else {
                format!("{} conflict(s) journaled: {named}", conflicts.len())
            },
            "0 conflicts",
        )
        .trend(conflicts.len() as f64, "conflicts")
        .build(),
    ]
}
