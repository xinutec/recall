//! The global capture pause, read from the single pause file. Port of the
//! read side of `src/recall/capture_control.py` — writing the file (pause /
//! resume / the bounded auto-clear) stays with the Python control plane; this
//! daemon only self-gates on it, like every other recording agent.

use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
use std::path::Path;

/// The pause file's name — a cross-process contract: the Python control plane
/// writes and clears it, every recording agent self-gates on it.
pub const PAUSE_FILE: &str = "capture_paused_until";

/// The recorded resume-by time, or `None` if not paused. A hand-written naive
/// timestamp is read as UTC rather than failing — this gates every capture
/// agent's main loop, so an unreadable file must mean "recording", never a
/// crash-loop.
pub fn paused_until(root: &Path) -> Option<DateTime<Utc>> {
    let text = std::fs::read_to_string(root.join(PAUSE_FILE)).ok()?;
    parse_pause_timestamp(text.trim())
}

/// The subset of ISO-8601 Python's `fromisoformat` accepts that has ever been
/// seen in this file: aware timestamps, naive timestamps (read as UTC), and a
/// bare date (midnight UTC).
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

pub fn is_paused(root: &Path, now: DateTime<Utc>) -> bool {
    paused_until(root).is_some_and(|until| until > now)
}
