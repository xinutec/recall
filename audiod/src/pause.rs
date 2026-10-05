//! The global capture pause: the single pause file every recording agent
//! self-gates on, and the local commands that write and clear it.

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};
use std::path::Path;

/// The pause file's name: a cross-process contract every recording agent
/// self-gates on.
pub const PAUSE_FILE: &str = "capture_paused_until";

/// The recorded resume-by time, or `None` if not paused. A hand-written naive
/// timestamp is read as UTC; an unreadable file means "recording", never a
/// crash loop.
pub fn paused_until(root: &Path) -> Option<DateTime<Utc>> {
    let text = std::fs::read_to_string(root.join(PAUSE_FILE)).ok()?;
    parse_pause_timestamp(text.trim())
}

/// The ISO-8601 forms this file holds: aware timestamps, naive timestamps
/// (read as UTC), and a bare date (midnight UTC).
fn parse_pause_timestamp(text: &str) -> Option<DateTime<Utc>> {
    if let Ok(aware) = DateTime::parse_from_rfc3339(text) {
        return Some(aware.with_timezone(&Utc));
    }
    if let Ok(naive) = NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f") {
        return Some(Utc.from_utc_datetime(&naive));
    }
    let date = NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()?;
    Some(Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0)?))
}

/// A pause lasts at most this long, so a forgotten one cannot leave the house
/// unrecorded indefinitely.
pub const MAX_PAUSE_HOURS: i64 = 24;

/// When a pause starting at `now` must end, clamped to [`MAX_PAUSE_HOURS`].
#[must_use]
pub fn resume_by(now: DateTime<Utc>, minutes: Option<i64>) -> DateTime<Utc> {
    let cap = Duration::hours(MAX_PAUSE_HOURS);
    let span = minutes.map_or(cap, |m| Duration::minutes(m.max(0)).min(cap));
    now + span
}

/// Begin a bounded pause; returns when it ends. The break-glass control for
/// when Isis cannot be reached: every recording agent gates on the file.
///
/// # Errors
/// If the file cannot be written, which means the pause did not take.
pub fn pause(
    root: &Path,
    now: DateTime<Utc>,
    minutes: Option<i64>,
) -> std::io::Result<DateTime<Utc>> {
    let until = resume_by(now, minutes);
    std::fs::write(
        root.join(PAUSE_FILE),
        until.to_rfc3339_opts(chrono::SecondsFormat::Micros, false),
    )?;
    Ok(until)
}

/// End a pause by clearing the file; parked agents resume. Only on a person's
/// command. An absent file is success.
///
/// # Errors
/// If the file exists and cannot be removed.
pub fn resume(root: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(root.join(PAUSE_FILE)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

pub fn is_paused(root: &Path, now: DateTime<Utc>) -> bool {
    paused_until(root).is_some_and(|until| until > now)
}
