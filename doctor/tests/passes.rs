//! The server's background passes: a pass failing or gone quiet fails, one
//! still in its long first run does not, one switched off skips.

use audiocore::record_health::{Failing, Pass, RecordHealth};
use chrono::{DateTime, Duration, Utc};
use doctor::check::Verdict;
use doctor::passes::pass_checks;

fn now() -> DateTime<Utc> {
    "2026-10-12T12:00:00Z".parse().expect("instant")
}

fn ago(minutes: i64) -> String {
    (now() - Duration::minutes(minutes)).to_rfc3339()
}

/// The view as it runs: every 30 s, started an hour ago, last pass a moment
/// ago and clean.
fn view() -> Pass {
    Pass {
        name: "view".to_owned(),
        every_s: 30,
        since_utc: ago(60),
        off: None,
        running_since_utc: None,
        last_end_utc: Some(ago(0)),
        failing: None,
    }
}

fn graded(passes: Vec<Pass>) -> Vec<(String, Verdict, String)> {
    let health = RecordHealth {
        passes,
        ..RecordHealth::default()
    };
    pass_checks(&Ok(health), now())
        .into_iter()
        .map(|c| (c.label, c.verdict, c.observed))
        .collect()
}

fn one(pass: Pass) -> (Verdict, String) {
    let got = graded(vec![pass]);
    assert_eq!(got.len(), 1, "{got:?}");
    (got[0].1, got[0].2.clone())
}

#[test]
fn a_pass_that_ends_clean_passes() {
    assert_eq!(one(view()).0, Verdict::Pass);
}

#[test]
fn a_pass_failing_past_its_allowance_fails_with_the_error() {
    // Every refresh refused, for hours.
    let (verdict, observed) = one(Pass {
        failing: Some(Failing {
            since_utc: ago(30),
            error: "UNIQUE constraint failed: rendered_lines.id".to_owned(),
        }),
        ..view()
    });
    assert_eq!(verdict, Verdict::Fail);
    assert!(observed.contains("UNIQUE constraint failed"), "{observed}");
}

#[test]
fn a_pass_failing_only_briefly_warns() {
    let (verdict, _) = one(Pass {
        failing: Some(Failing {
            since_utc: ago(2),
            error: "database is locked".to_owned(),
        }),
        ..view()
    });
    assert_eq!(verdict, Verdict::Warn);
}

#[test]
fn a_pass_gone_quiet_fails() {
    // Its last pass ended 20 min ago; it runs every 30 s, allowance 10 min.
    let (verdict, observed) = one(Pass {
        last_end_utc: Some(ago(20)),
        ..view()
    });
    assert_eq!(verdict, Verdict::Fail);
    assert!(observed.contains("20 min"), "{observed}");
}

#[test]
fn the_allowance_is_three_intervals_when_that_is_longer() {
    // Every 10 min: 25 min since the last pass is within 30.
    let pass = Pass {
        name: "segment-derive".to_owned(),
        every_s: 600,
        last_end_utc: Some(ago(25)),
        ..view()
    };
    assert_eq!(one(pass.clone()).0, Verdict::Pass);
    assert_eq!(
        one(Pass {
            last_end_utc: Some(ago(35)),
            ..pass
        })
        .0,
        Verdict::Fail
    );
}

#[test]
fn a_long_first_run_passes_until_an_hour_then_fails() {
    // The start-up render: running since start, nothing ended yet.
    let running = |minutes| Pass {
        since_utc: ago(minutes),
        running_since_utc: Some(ago(minutes)),
        last_end_utc: None,
        ..view()
    };
    assert_eq!(one(running(35)).0, Verdict::Pass);
    let (verdict, observed) = one(running(70));
    assert_eq!(verdict, Verdict::Fail);
    assert!(observed.contains("70 min"), "{observed}");
}

#[test]
fn a_pass_that_never_started_fails_once_its_allowance_is_up() {
    let never = |minutes| Pass {
        since_utc: ago(minutes),
        running_since_utc: None,
        last_end_utc: None,
        ..view()
    };
    assert_eq!(one(never(5)).0, Verdict::Pass);
    assert_eq!(one(never(15)).0, Verdict::Fail);
}

#[test]
fn a_pass_switched_off_skips_and_says_why() {
    let (verdict, observed) = one(Pass {
        off: Some("no AVX2".to_owned()),
        last_end_utc: None,
        ..view()
    });
    assert_eq!(verdict, Verdict::Skip);
    assert!(observed.contains("no AVX2"), "{observed}");
}

#[test]
fn each_pass_is_its_own_check_with_a_stable_label() {
    let got = graded(vec![
        view(),
        Pass {
            name: "enrol".to_owned(),
            every_s: 600,
            ..view()
        },
    ]);
    let labels: Vec<&str> = got.iter().map(|g| g.0.as_str()).collect();
    assert_eq!(labels, ["background pass: view", "background pass: enrol"]);
}
