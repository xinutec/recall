//! What a background pass tells the doctor: when it ran, and since when it
//! has been failing, which a run of failures does not move.

use chrono::{DateTime, Duration, Utc};
use recalld::passes::Passes;

fn at(s: i64) -> DateTime<Utc> {
    let start: DateTime<Utc> = "2026-10-12T09:19:00Z".parse().expect("instant");
    start + Duration::seconds(s)
}

#[test]
fn a_failing_pass_keeps_when_it_started_failing_until_a_clean_pass() {
    let passes = Passes::new();
    passes.register("view", std::time::Duration::from_secs(30), at(0));

    passes.begin("view", at(1));
    let running = passes.report();
    assert_eq!(
        running[0].running_since_utc.as_deref(),
        Some("2026-10-12T09:19:01Z")
    );

    passes.ended("view", at(2), Some("UNIQUE constraint failed".to_owned()));
    passes.ended(
        "view",
        at(32),
        Some("UNIQUE constraint failed again".to_owned()),
    );
    let failing = passes.report()[0].failing.clone().expect("failing");
    assert_eq!(
        failing.since_utc, "2026-10-12T09:19:02Z",
        "from the first failure"
    );
    assert_eq!(
        failing.error, "UNIQUE constraint failed again",
        "the newest error"
    );
    assert_eq!(passes.report()[0].running_since_utc, None);

    passes.ended("view", at(62), None);
    let clean = &passes.report()[0];
    assert_eq!(clean.failing, None);
    assert_eq!(clean.last_end_utc.as_deref(), Some("2026-10-12T09:20:02Z"));
}

#[test]
fn a_pass_switched_off_says_why_and_an_unknown_name_changes_nothing() {
    let passes = Passes::new();
    passes.off(
        "speech",
        std::time::Duration::from_mins(2),
        at(0),
        "no AVX2".to_owned(),
    );
    passes.ended("nobody", at(1), Some("ignored".to_owned()));

    let report = passes.report();
    assert_eq!(report.len(), 1);
    assert_eq!(report[0].off.as_deref(), Some("no AVX2"));
    assert_eq!(report[0].every_s, 120);
}

#[tokio::test]
async fn a_run_reports_its_end_and_hands_back_only_a_result() {
    let passes = Passes::new();
    passes.register("view", std::time::Duration::from_secs(30), at(0));

    let clean = passes.run("view", || Ok::<_, String>(3), |_| None).await;
    assert_eq!(clean, Some(3));
    let report = &passes.report()[0];
    assert_eq!(report.failing, None);
    assert_eq!(report.running_since_utc, None);
    assert!(report.last_end_utc.is_some());

    let partly = passes
        .run(
            "view",
            || Ok::<_, String>(1),
            |n| Some(format!("{n} clip(s) would not render")),
        )
        .await;
    assert_eq!(partly, Some(1), "a run that finished hands back its result");
    let failing = passes.report()[0].failing.clone().expect("failing");
    assert_eq!(failing.error, "1 clip(s) would not render");

    let erred = passes
        .run("view", || Err::<u32, _>("database is locked"), |_| None)
        .await;
    assert_eq!(erred, None);
    let failing = passes.report()[0].failing.clone().expect("failing");
    assert_eq!(failing.error, "database is locked");
}

#[tokio::test]
async fn a_run_that_panics_is_a_failure_not_a_dead_loop() {
    let passes = Passes::new();
    passes.register("enrol", std::time::Duration::from_mins(10), at(0));

    let panicked = passes
        .run(
            "enrol",
            || -> Result<u32, String> { panic!("index out of bounds") },
            |_| None,
        )
        .await;
    assert_eq!(panicked, None);
    let failing = passes.report()[0].failing.clone().expect("failing");
    assert!(failing.error.contains("panicked"), "{}", failing.error);
    assert_eq!(passes.report()[0].running_since_utc, None);
}
