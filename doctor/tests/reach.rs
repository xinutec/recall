//! Whether the server answers: one miss warns (a restart takes two minutes),
//! a quarter of an hour of them fails.

use chrono::{DateTime, Duration, Utc};
use doctor::check::Verdict;
use doctor::reach::reach_check;

fn at(minutes: i64) -> DateTime<Utc> {
    let start: DateTime<Utc> = "2026-10-12T13:50:00Z".parse().expect("instant");
    start + Duration::minutes(minutes)
}

#[test]
fn an_answer_passes_and_is_remembered() {
    let dir = tempfile::tempdir().expect("tmp");
    let state = dir.path().join("fleet-reached");

    let check = reach_check(&state, at(0), &Ok(()));

    assert_eq!(check.verdict, Verdict::Pass);
    assert!(state.exists());
}

#[test]
fn a_miss_shortly_after_an_answer_warns_and_a_long_silence_fails() {
    // An outage: answered at 13:50, then 502 until 14:19.
    let dir = tempfile::tempdir().expect("tmp");
    let state = dir.path().join("fleet-reached");
    let _ = reach_check(&state, at(0), &Ok(()));
    let missed = Err("the fleet answered 502".to_owned());

    let early = reach_check(&state, at(5), &missed);
    assert_eq!(early.verdict, Verdict::Warn);

    let late = reach_check(&state, at(20), &missed);
    assert_eq!(late.verdict, Verdict::Fail);
    assert!(late.observed.contains("502"), "{}", late.observed);
    assert!(late.observed.contains("20 min"), "{}", late.observed);

    assert_eq!(reach_check(&state, at(25), &Ok(())).verdict, Verdict::Pass);
    assert_eq!(reach_check(&state, at(30), &missed).verdict, Verdict::Warn);
}

#[test]
fn a_miss_with_no_answer_on_record_warns() {
    let dir = tempfile::tempdir().expect("tmp");
    let state = dir.path().join("fleet-reached");

    let check = reach_check(&state, at(0), &Err("connection refused".to_owned()));

    assert_eq!(check.verdict, Verdict::Warn);
}
