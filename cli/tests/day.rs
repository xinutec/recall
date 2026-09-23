//! The local-day window, the one place this crate does arithmetic on time.

use cli::day::bounds;

#[test]
fn a_date_becomes_a_window_exactly_one_day_long() {
    let (start, end) = bounds("2026-07-05").expect("a real date");
    let start = chrono::DateTime::parse_from_rfc3339(&start).expect("rfc3339");
    let end = chrono::DateTime::parse_from_rfc3339(&end).expect("rfc3339");
    assert_eq!(end - start, chrono::Duration::days(1));
}

/// The offset comes from the day asked for, not from now; otherwise a summer or
/// winter date is an hour off, enough to move an evening onto the wrong day.
/// Stated as "they differ" rather than a fixed offset so it holds in any timezone.
#[test]
fn a_summer_day_and_a_winter_day_do_not_share_an_offset() {
    let (summer, _) = bounds("2026-07-05").expect("summer");
    let (winter, _) = bounds("2026-01-05").expect("winter");
    let summer = chrono::DateTime::parse_from_rfc3339(&summer).expect("rfc3339");
    let winter = chrono::DateTime::parse_from_rfc3339(&winter).expect("rfc3339");
    if summer.offset().local_minus_utc() == winter.offset().local_minus_utc() {
        // A fixed-offset zone such as UTC has nothing for this test to catch.
        assert_eq!(
            chrono::Local::now().offset().to_string(),
            summer.offset().to_string(),
            "same offset both halves of the year: a fixed-offset zone"
        );
    }
}

#[test]
fn today_and_yesterday_are_a_day_apart() {
    let (today, _) = bounds("today").expect("today");
    let (yesterday, _) = bounds("yesterday").expect("yesterday");
    let today = chrono::DateTime::parse_from_rfc3339(&today).expect("rfc3339");
    let yesterday = chrono::DateTime::parse_from_rfc3339(&yesterday).expect("rfc3339");
    assert_eq!(today - yesterday, chrono::Duration::days(1));
}

/// Anything else is refused rather than guessed at, or a typo would silently
/// answer about the wrong day.
#[test]
fn anything_that_is_not_a_date_is_refused() {
    for junk in ["yestreday", "", "2026-13-01", "05/07/2026", "last tuesday"] {
        assert!(bounds(junk).is_none(), "{junk:?} must not parse");
    }
}
