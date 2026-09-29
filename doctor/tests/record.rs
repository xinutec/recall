//! The record's own faults, graded: a failed request and a minute shown twice
//! are each a failure for the user to hear about, not for them to find.

use audiocore::record_health::{Doubled, DoubledMinute, Fault, Faults, RecordHealth};
use doctor::check::Verdict;
use doctor::record::record_checks;

fn verdicts(health: &Result<RecordHealth, String>) -> Vec<(String, Verdict, String)> {
    record_checks(health)
        .into_iter()
        .map(|c| (c.label, c.verdict, c.observed))
        .collect()
}

#[test]
fn a_clean_record_passes_both() {
    let got = verdicts(&Ok(RecordHealth::default()));

    assert!(got.iter().all(|(_, v, _)| *v == Verdict::Pass), "{got:?}");
    assert_eq!(got.len(), 2);
}

#[test]
fn one_failed_save_fails_and_names_the_route_the_error_and_the_time() {
    let health = RecordHealth {
        faults: Faults {
            count: 1,
            last: Some(Fault {
                utc: "2026-09-28T15:25:53Z".to_owned(),
                what: "correct".to_owned(),
                error: "database is locked".to_owned(),
            }),
        },
        ..RecordHealth::default()
    };

    let got = verdicts(&Ok(health));

    let (_, verdict, observed) = &got[0];
    assert_eq!(*verdict, Verdict::Fail);
    for part in ["correct", "database is locked", "2026-09-28T15:25:53Z"] {
        assert!(observed.contains(part), "{observed}");
    }
    assert_eq!(got[1].1, Verdict::Pass);
}

#[test]
fn a_minute_shown_twice_fails_and_names_the_mic() {
    let health = RecordHealth {
        doubled: Doubled {
            count: 3,
            last: Some(DoubledMinute {
                source: "pixel5".to_owned(),
                start_utc: "2026-09-28T13:16:15+00:00".to_owned(),
            }),
        },
        ..RecordHealth::default()
    };

    let got = verdicts(&Ok(health));

    let (_, verdict, observed) = &got[1];
    assert_eq!(*verdict, Verdict::Fail);
    assert!(
        observed.contains("3 minute") && observed.contains("pixel5"),
        "{observed}"
    );
}

#[test]
fn an_unreachable_fleet_skips_both_saying_why() {
    let got = verdicts(&Err("cannot reach the fleet (timeout)".to_owned()));

    assert!(
        got.iter()
            .all(|(_, v, o)| *v == Verdict::Skip && o.contains("timeout")),
        "{got:?}"
    );
}

#[test]
fn the_labels_stay_the_same_whatever_the_counts() {
    let clean = record_checks(&Ok(RecordHealth::default()));
    let broken = record_checks(&Err("x".to_owned()));

    let labels = |c: &[doctor::check::Check]| c.iter().map(|c| c.label.clone()).collect::<Vec<_>>();
    assert_eq!(labels(&clean), labels(&broken));
}
