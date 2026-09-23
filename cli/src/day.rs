//! Turning a date a person typed into the window the archive is asked about.
//!
//! ⚠ The local day, using the UTC offset in force on that day, not today's.
//! The archive stores UTC; with today's offset, a date across a daylight-saving
//! change would shift by an hour and move late-evening conversations to the
//! next day.

use chrono::{Duration, Local, NaiveDate, TimeZone};

/// The two instants bounding the local day `date` names, as RFC 3339.
///
/// Accepts `YYYY-MM-DD`, `today` and `yesterday`; `None` for anything else.
#[must_use]
pub fn bounds(date: &str) -> Option<(String, String)> {
    let day = match date {
        "today" => Local::now().date_naive(),
        "yesterday" => Local::now().date_naive() - Duration::days(1),
        _ => NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?,
    };
    // `earliest()`, because a local midnight can be ambiguous when the clocks
    // move; the earliest instant keeps the window a superset of the day, since
    // a duplicated conversation is visible and a missing one is not.
    let start = Local
        .from_local_datetime(&day.and_hms_opt(0, 0, 0)?)
        .earliest()?;
    let end = start + Duration::days(1);
    Some((start.to_rfc3339(), end.to_rfc3339()))
}
