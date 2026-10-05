//! Telling a deliberate pause from lost speech.

use chrono::{DateTime, Duration, Utc};
use doctor::check::Verdict;
use doctor::loss::{Event, Gap, PAUSE, RESUME, active_spans, loss_checks, uncovered_loss};
use doctor::source::SourceKind;

fn at(minute: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_757_000_000 + minute * 60, 0).unwrap()
}

fn event(kind: &str, minute: i64) -> Event {
    Event {
        utc: at(minute),
        kind: kind.to_owned(),
        source_id: None,
    }
}

#[test]
fn no_events_makes_no_claim() {
    assert!(
        uncovered_loss(
            &[],
            &[],
            "usb",
            at(100),
            Duration::minutes(2),
            Duration::zero()
        )
        .is_empty()
    );
}

#[test]
fn a_span_with_no_audio_at_all_is_the_whole_span() {
    // A crash loop: no segments, so no gap between segments.
    let events = [event(RESUME, 0), event(PAUSE, 60)];
    let losses = uncovered_loss(
        &[],
        &events,
        "usb",
        at(100),
        Duration::minutes(2),
        Duration::zero(),
    );
    assert_eq!(losses.len(), 1);
    assert_eq!(losses[0].start, at(0));
    assert_eq!(losses[0].end, at(60));
}

#[test]
fn a_deliberate_pause_is_not_loss() {
    let events = [
        event(RESUME, 0),
        event(PAUSE, 10),
        event(RESUME, 40),
        event(PAUSE, 50),
    ];
    let intervals = [(at(0), at(10)), (at(40), at(50))];
    assert!(
        uncovered_loss(
            &intervals,
            &events,
            "usb",
            at(100),
            Duration::minutes(2),
            Duration::zero()
        )
        .is_empty()
    );
}

#[test]
fn slop_under_min_loss_is_absorbed() {
    // The first segment starts a minute after the resume.
    let events = [event(RESUME, 0), event(PAUSE, 60)];
    let intervals = [(at(1), at(60))];
    assert!(
        uncovered_loss(
            &intervals,
            &events,
            "usb",
            at(100),
            Duration::minutes(2),
            Duration::zero()
        )
        .is_empty()
    );
}

#[test]
fn the_settle_horizon_keeps_a_running_captures_tail_out_of_it() {
    let events = [event(RESUME, 0)];
    let intervals = [(at(0), at(50))];
    let losses = uncovered_loss(
        &intervals,
        &events,
        "usb",
        at(55),
        Duration::minutes(2),
        Duration::minutes(10),
    );
    assert!(losses.is_empty(), "{losses:?}");
}

#[test]
fn overlapping_coverage_merges_rather_than_double_counting() {
    let events = [event(RESUME, 0), event(PAUSE, 60)];
    let intervals = [(at(0), at(30)), (at(20), at(60))];
    assert!(
        uncovered_loss(
            &intervals,
            &events,
            "usb",
            at(100),
            Duration::minutes(2),
            Duration::zero()
        )
        .is_empty()
    );
}

#[test]
fn a_phone_warns_where_the_wired_mic_fails() {
    let gap = Gap {
        source_id: "usb".to_owned(),
        start: at(0),
        end: at(30),
    };
    let sources = [
        ("usb".to_owned(), SourceKind::CoreAudio),
        ("pixel9".to_owned(), SourceKind::TcpPcm),
    ];
    let checks = loss_checks(std::slice::from_ref(&gap), &sources, Duration::hours(48));
    let usb = checks
        .iter()
        .find(|c| c.label == "speech-loss:usb")
        .unwrap();
    assert_eq!(usb.verdict, Verdict::Fail);

    let phone_gap = Gap {
        source_id: "pixel9".to_owned(),
        ..gap
    };
    let checks = loss_checks(&[phone_gap], &sources, Duration::hours(48));
    let phone = checks
        .iter()
        .find(|c| c.label == "speech-loss:pixel9")
        .unwrap();
    assert_eq!(phone.verdict, Verdict::Warn);
    let roll = checks.iter().find(|c| c.label == "speech-loss").unwrap();
    assert_eq!(roll.verdict, Verdict::Warn);
}

#[test]
fn loss_on_an_unregistered_source_gets_its_own_line() {
    // With no kind on record, the strict verdict.
    let gap = Gap {
        source_id: "ghost".to_owned(),
        start: at(0),
        end: at(30),
    };
    let checks = loss_checks(&[gap], &[], Duration::hours(48));
    let ghost = checks
        .iter()
        .find(|c| c.label == "speech-loss:ghost")
        .unwrap();
    assert_eq!(ghost.verdict, Verdict::Fail);
}

#[test]
fn a_clean_window_says_so_per_device_and_once_overall() {
    let sources = [("usb".to_owned(), SourceKind::CoreAudio)];
    let checks = loss_checks(&[], &sources, Duration::hours(48));
    assert_eq!(checks.len(), 2);
    assert!(checks.iter().all(|c| c.verdict == Verdict::Pass));
    assert_eq!(checks[1].observed, "no unexplained loss in 48h");
}

#[test]
fn a_span_opens_on_a_resume_and_never_before_the_first_one() {
    let now = at(100);
    let events = [event(PAUSE, 5), event(RESUME, 10), event(PAUSE, 20)];
    let spans = active_spans(&events, now);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].start, at(10));
    assert_eq!(spans[0].end, at(20));
}

#[test]
fn a_trailing_resume_stays_open_to_now() {
    let now = at(100);
    let spans = active_spans(&[event(RESUME, 10)], now);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].end, now);
}

#[test]
fn a_repeated_resume_does_not_reopen_an_already_open_span() {
    let spans = active_spans(&[event(RESUME, 10), event(RESUME, 20)], at(100));
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].start, at(10));
}
