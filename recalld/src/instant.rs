//! One spelling of an ISO-8601 instant, matching the Python that wrote every
//! stored timestamp in this database.
//!
//! ⚠ **This is not cosmetic.** `start_utc` is compared and ordered as TEXT
//! (`WHERE t.start_utc < ?1 ORDER BY t.start_utc`), so two spellings of the same
//! moment are two different values to every query. A turn stored as
//! `...T09:51:01Z` sorts before one stored as `...T09:51:01+00:00` and lands in
//! the wrong page.
//!
//! What the Python does, and therefore what this does:
//!
//! - `Z` becomes `+00:00` — the same instant, the sortable spelling.
//! - A missing offset is assumed UTC rather than refused.
//! - **A non-UTC offset is KEPT, not converted.** `+01:00` stays `+01:00`.
//!   Converting to UTC would be tidier and would be wrong: it would not match
//!   what is already in the table, and this port must not quietly re-base
//!   timestamps a person can page through.
//! - The fraction is omitted when it is zero, and six digits when it is not,
//!   which is `datetime.isoformat()`'s rule. chrono's default trims trailing
//!   zeros (`.5` → `.500`) and would disagree with every row already stored.

use chrono::{DateTime, FixedOffset, NaiveDateTime, SecondsFormat, TimeZone, Utc};

/// Re-spell an ISO-8601 instant the way `datetime.isoformat()` would, or `None`
/// if it does not parse.
pub fn python_isoformat(value: &str) -> Option<String> {
    let parsed = parse(value)?;
    let format = if parsed.timestamp_subsec_micros() == 0 {
        SecondsFormat::Secs
    } else {
        SecondsFormat::Micros
    };
    Some(parsed.to_rfc3339_opts(format, false))
}

/// Parse an instant the way `datetime.fromisoformat` does. Public because a
/// caller that must COMPARE two stored timestamps needs the instant, while one
/// that must re-emit one needs [`python_isoformat`] — and doing the second by
/// hand is how a spelling drifts.
pub fn parse(value: &str) -> Option<DateTime<FixedOffset>> {
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Some(parsed);
    }
    // No offset at all: the Python assumes UTC rather than refusing, and a
    // recorder that forgets its offset should not lose its turn.
    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(value, format) {
            return Some(Utc.from_utc_datetime(&naive).fixed_offset());
        }
    }
    None
}
