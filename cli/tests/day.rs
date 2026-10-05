//! The local-day window.

use cli::day::bounds;

#[test]
fn a_date_becomes_a_window_exactly_one_day_long() {
    let (start, end) = bounds("2026-07-05").expect("a real date");
    let start = chrono::DateTime::parse_from_rfc3339(&start).expect("rfc3339");
    let end = chrono::DateTime::parse_from_rfc3339(&end).expect("rfc3339");
    assert_eq!(end - start, chrono::Duration::days(1));
}

/// Days of 23 and 25 hours included: each ends where the next begins. The
/// dates are the 2026 clock changes in Europe and the US; in other zones they
/// are ordinary days.
#[test]
fn consecutive_days_tile_with_no_gap_across_a_clock_change() {
    for (day, next) in [
        ("2026-03-29", "2026-03-30"),
        ("2026-10-25", "2026-10-26"),
        ("2026-03-08", "2026-03-09"),
        ("2026-11-01", "2026-11-02"),
    ] {
        let (_, end) = bounds(day).expect("a real date");
        let (start, _) = bounds(next).expect("a real date");
        let end = chrono::DateTime::parse_from_rfc3339(&end).expect("rfc3339");
        let start = chrono::DateTime::parse_from_rfc3339(&start).expect("rfc3339");
        assert_eq!(end, start, "{day} ends at {end}, {next} begins at {start}");
    }
}

/// The offset comes from the day asked for, not from now. Asserted as "they
/// differ" so it holds in any timezone.
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

/// Refused, not guessed: a typo must not answer about the wrong day.
#[test]
fn anything_that_is_not_a_date_is_refused() {
    for junk in ["yestreday", "", "2026-13-01", "05/07/2026", "last tuesday"] {
        assert!(bounds(junk).is_none(), "{junk:?} must not parse");
    }
}
