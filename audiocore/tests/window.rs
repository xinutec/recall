//! A recording window: off unless a person opened one, and only until its end.

use audiocore::capture::Window;
use chrono::{DateTime, Duration, Utc};

fn at(text: &str) -> DateTime<Utc> {
    audiocore::instant::parse_utc(text).expect("instant")
}

#[test]
fn each_window_reads_back_as_it_was_spelled() {
    let until = at("2026-10-23T11:00:00Z");
    for window in [Window::Off, Window::Record(until), Window::Hold(until)] {
        assert_eq!(Window::parse(&window.spell()), Some(window));
    }
    assert_eq!(Window::Record(until).spell(), "record 2026-10-23T11:00:00Z");
}

#[test]
fn unreadable_text_is_no_window_at_all() {
    for text in ["", "on", "record", "record soon", "2026-10-23T11:00:00Z"] {
        assert_eq!(Window::parse(text), None, "{text:?}");
    }
}

#[test]
fn only_an_open_window_records_and_it_closes_at_its_end() {
    let now = at("2026-10-23T10:00:00Z");
    let until = now + Duration::hours(1);
    assert!(Window::Record(until).records(now));
    assert!(!Window::Record(until).records(until));
    assert!(!Window::Hold(until).records(now));
    assert!(!Window::Off.records(now));
}

#[test]
fn a_window_past_its_end_is_off() {
    let now = at("2026-10-23T10:00:00Z");
    assert_eq!(Window::Record(now).at(now), Window::Off);
    assert_eq!(Window::Hold(now).at(now), Window::Off);
    let later = now + Duration::minutes(1);
    assert_eq!(Window::Hold(later).at(now), Window::Hold(later));
}

#[test]
fn a_window_is_opened_for_at_most_a_day() {
    let now = at("2026-10-23T10:00:00Z");
    assert_eq!(
        Window::open(now, now + Duration::hours(30)),
        Window::Record(now + Duration::hours(24))
    );
    assert_eq!(Window::open(now, now - Duration::hours(1)), Window::Off);
}

#[test]
fn pausing_keeps_the_end_and_unpausing_resumes_it() {
    let now = at("2026-10-23T10:00:00Z");
    let until = now + Duration::hours(1);
    assert_eq!(Window::Record(until).paused(), Window::Hold(until));
    assert_eq!(Window::Hold(until).unpaused(), Window::Record(until));
    assert_eq!(Window::Off.paused(), Window::Off);
    assert_eq!(Window::Off.unpaused(), Window::Off);
}

fn state(running: bool, until: Option<&str>, held: bool) -> audiocore::capture::CaptureState {
    audiocore::capture::CaptureState {
        running,
        until: until.map(ToOwned::to_owned),
        held,
        desired_running: running,
        desired_until: until.map(ToOwned::to_owned),
        desired_held: held,
        settled: true,
        mic_reachable: true,
        state_token: String::new(),
    }
}

#[test]
fn the_state_a_recorder_polls_names_its_window() {
    let until = at("2026-10-23T11:00:00Z");
    let open = Some("2026-10-23T11:00:00Z");
    assert_eq!(
        state(false, None, false).desired_window(),
        Some(Window::Off)
    );
    assert_eq!(
        state(true, open, false).desired_window(),
        Some(Window::Record(until))
    );
    assert_eq!(
        state(false, open, true).desired_window(),
        Some(Window::Hold(until))
    );
    // Recording with no end is the fleet's own fault, and records.
    assert_eq!(state(true, None, false).desired_window(), None);
}
