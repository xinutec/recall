//! The record's own faults, for the doctor: requests that failed on our side
//! ([`crate::route`] keeps them), and minutes a microphone shows twice.
//!
//! The fleet measures, the doctor grades (`doctor/src/record.rs`), as for the
//! live tier.

use crate::same_speech::{COPY_SECONDS, same_span};
use audiocore::record_health::{Doubled, DoubledMinute, Fault, Faults, RecordHealth};
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use std::path::Path;

crate::statements! {
    /// Pairs of one mic's clips since ?1 starting within ?2 seconds of each
    /// other, both with lines showing: the candidates [`doubled_minutes`]
    /// holds to [`same_span`]. Newest first.
    COPY_CANDIDATES: Meaning =
        "SELECT a.source_id, a.start_utc, a.end_utc, b.start_utc, b.end_utc
         FROM audio_segments a
         JOIN audio_segments b
           ON b.source_id = a.source_id AND b.id > a.id
          AND b.start_utc BETWEEN strftime('%Y-%m-%dT%H:%M:%S', a.start_utc, printf('-%d seconds', ?2))
                              AND strftime('%Y-%m-%dT%H:%M:%S', a.start_utc, printf('+%d seconds', ?2 + 1))
         WHERE a.start_utc >= ?1
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
    let parse = |raw: &str| audiocore::instant::parse_utc(raw);
    let mut stmt = COPY_CANDIDATES.prepare(meaning)?;
    let rows = stmt.query_map(
        (
            audiocore::instant::python_isoformat_utc(since),
            COPY_SECONDS,
        ),
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        },
    )?;
    let mut minutes = Vec::new();
    for row in rows {
        let (source, a_start, a_end, b_start, b_end) = row?;
        let spans = (
            parse(&a_start).zip(parse(&a_end)),
            parse(&b_start).zip(parse(&b_end)),
        );
        if let (Some(a), Some(b)) = spans
            && same_span(a, b)
        {
            minutes.push(DoubledMinute {
                source,
                start_utc: a_start,
            });
        }
    }
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
