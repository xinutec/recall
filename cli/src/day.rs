//! A date a person typed, as the window of UTC instants it covers locally.

use chrono::{DateTime, Duration, Local, NaiveDate, TimeZone};

/// The local midnights starting `date` and the day after, as RFC 3339, each
/// with the offset in force then. A day across a clock change is 23 or 25
/// hours long.
///
/// Accepts `YYYY-MM-DD`, `today` and `yesterday`; `None` for anything else.
#[must_use]
pub fn bounds(date: &str) -> Option<(String, String)> {
    let day = match date {
        "today" => Local::now().date_naive(),
        "yesterday" => Local::now().date_naive() - Duration::days(1),
        _ => NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?,
    };
    let start = midnight(day)?;
    let end = midnight(day.succ_opt()?)?;
    Some((start.to_rfc3339(), end.to_rfc3339()))
}

/// The local midnight starting `day`. If the clocks move then, the earliest
/// such instant.
fn midnight(day: NaiveDate) -> Option<DateTime<Local>> {
    Local
        .from_local_datetime(&day.and_hms_opt(0, 0, 0)?)
        .earliest()
}
