//! The record's own faults, for the doctor: requests that failed on our side
//! ([`crate::route`] keeps them), and minutes a microphone shows twice.
//!
//! The fleet measures, the doctor grades (`doctor/src/record.rs`), as for the
//! live tier.

use audiocore::record_health::{Doubled, DoubledMinute, Fault, Faults, RecordHealth};
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use std::path::Path;

crate::statements! {
    /// Minutes since ?1 whose speech one mic shows from two clips: starts
    /// within ?2 seconds, overlapping by more than half the shorter, both with
    /// lines showing. Newest first.
    DOUBLED: Meaning =
        "SELECT a.source_id, a.start_utc FROM audio_segments a
         JOIN audio_segments b
           ON b.source_id = a.source_id AND b.id > a.id
          AND b.start_utc BETWEEN strftime('%Y-%m-%dT%H:%M:%S', a.start_utc, printf('-%d seconds', ?2))
                              AND strftime('%Y-%m-%dT%H:%M:%S', a.start_utc, printf('+%d seconds', ?2 + 1))
         WHERE a.start_utc >= ?1
           AND julianday(min(a.end_utc, b.end_utc)) - julianday(max(a.start_utc, b.start_utc))
             > min(julianday(a.end_utc) - julianday(a.start_utc),
                   julianday(b.end_utc) - julianday(b.start_utc)) / 2
           AND EXISTS (SELECT 1 FROM transcript_segments t WHERE t.audio_segment_id = a.id
                         AND t.superseded_by IS NULL AND t.hidden_reason IS NULL)
           AND EXISTS (SELECT 1 FROM transcript_segments t WHERE t.audio_segment_id = b.id
                         AND t.superseded_by IS NULL AND t.hidden_reason IS NULL)
         ORDER BY a.start_utc DESC";
}

/// The fault log's name, in the data root's `logs/`.
pub const FAULT_LOG: &str = "faults.jsonl";

/// The faults kept since `since`. No log yet is no faults; a line that does
/// not read is skipped, since a torn last line must not hide the rest.
///
/// # Errors
/// If the log exists and cannot be read.
pub fn faults_since(log: &Path, since: DateTime<Utc>) -> std::io::Result<Faults> {
    let text = match std::fs::read_to_string(log) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Faults::default()),
        Err(err) => return Err(err),
    };
    let mut faults = Faults::default();
    for fault in text
        .lines()
        .filter_map(|l| serde_json::from_str::<Fault>(l).ok())
    {
        let recent = DateTime::parse_from_rfc3339(&fault.utc).is_ok_and(|t| t >= since);
        if recent {
            faults.count += 1;
            faults.last = Some(fault);
        }
    }
    Ok(faults)
}

/// The minutes shown twice since `since`.
///
/// # Errors
/// If the database refuses.
pub fn doubled_minutes(meaning: &Connection, since: DateTime<Utc>) -> rusqlite::Result<Doubled> {
    let mut stmt = DOUBLED.prepare(meaning)?;
    let rows = stmt.query_map(
        (
            audiocore::instant::python_isoformat_utc(since),
            crate::queue::COPY_SECONDS,
        ),
        |r| {
            Ok(DoubledMinute {
                source: r.get(0)?,
                start_utc: r.get(1)?,
            })
        },
    )?;
    let minutes: Vec<DoubledMinute> = rows.collect::<rusqlite::Result<_>>()?;
    Ok(Doubled {
        count: minutes.len(),
        last: minutes.into_iter().next(),
    })
}

/// Both, for the route.
///
/// # Errors
/// If the database refuses or the fault log cannot be read.
pub fn measure(root: &Path, since: DateTime<Utc>) -> rusqlite::Result<RecordHealth> {
    let faults = faults_since(&root.join("logs").join(FAULT_LOG), since)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
    let meaning = crate::reads::open(root)?;
    Ok(RecordHealth {
        faults,
        doubled: doubled_minutes(&meaning, since)?,
    })
}
