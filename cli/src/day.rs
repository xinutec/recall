//! Turning a date a person typed into the window the archive is asked about.
//!
//! ⚠ **The LOCAL day, built from the offset ON THAT DAY — not today's.** The
//! archive stores UTC and a person asking for "the 5th" means the day they
//! lived. Half the year separates this household's two offsets, so using the
//! current offset to bound a date six months away shifts both ends by an hour:
//! quietly enough that a late-evening conversation falls off one day and onto
//! the next, and nothing in the output says so.

use chrono::{Duration, Local, NaiveDate, TimeZone};

/// The two instants bounding the local day `date` names, as RFC 3339.
///
/// Accepts `YYYY-MM-DD`, `today` and `yesterday`. `None` means it is none of
/// those, which the caller reports rather than guessing at.
#[must_use]
pub fn bounds(date: &str) -> Option<(String, String)> {
    let day = match date {
        "today" => Local::now().date_naive(),
        "yesterday" => Local::now().date_naive() - Duration::days(1),
        _ => NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?,
    };
    // ⚠ `earliest()`, because a local midnight can be ambiguous or absent when
    // the clocks move. Taking the earliest instant that answers to it keeps the
    // window a superset of the day rather than skipping its first hour — the
    // failure worth avoiding here is a MISSING conversation, not a duplicated
    // one, and a duplicate is visible where an omission is not.
    let start = Local
        .from_local_datetime(&day.and_hms_opt(0, 0, 0)?)
        .earliest()?;
    let end = start + Duration::days(1);
    Some((start.to_rfc3339(), end.to_rfc3339()))
}
