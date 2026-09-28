//! The record's own faults, read from the fleet and graded here: requests the
//! server failed, and minutes a microphone shows twice.
//!
//! Both went unnoticed for days before (failed saves in the app's log from 26
//! Sept, doubled phone minutes since early Sept) until Pippijn met them. Any
//! of either is a failure: each is a bug, and none is expected.

use crate::check::{Check, Verdict, check};
use audiocore::record_health::RecordHealth;
use chrono::{DateTime, Duration, Utc};

/// How far back a failed request counts: a day, so one clears on its own once
/// the cause is fixed.
#[must_use]
pub fn fault_window() -> Duration {
    Duration::hours(24)
}

/// How far back, by recording time, a doubled minute counts. Longer than the
/// fault window: a clip can be transcribed days after it was recorded.
#[must_use]
pub fn doubled_window() -> Duration {
    Duration::days(14)
}

const FAULTS: &str = "no request failed on the server";
const FAULTS_EXPECTED: &str = "every save and page load in the last day answered";
const DOUBLED: &str = "no minute is shown twice";
const DOUBLED_EXPECTED: &str = "each mic's minute is transcribed once";

/// Ask the fleet: faults over [`fault_window`], doubled minutes over
/// [`doubled_window`].
///
/// # Errors
/// A message for a skip line, naming what failed.
pub fn fetch(fleet: &crate::live::Fleet, now: DateTime<Utc>) -> Result<RecordHealth, String> {
    let ask = |since: DateTime<Utc>| {
        crate::live::get(fleet, "/sync/record/health")
            .query("since", &audiocore::instant::python_isoformat_utc(since))
            .call()
            .map_err(crate::live::describe)?
            .into_json::<RecordHealth>()
            .map_err(|e| format!("the fleet's answer did not parse ({e})"))
    };
    let faults = ask(now - fault_window())?.faults;
    let doubled = ask(now - doubled_window())?.doubled;
    Ok(RecordHealth { faults, doubled })
}

/// Both checks, from whatever the fleet said.
#[must_use]
pub fn record_checks(fetched: &Result<RecordHealth, String>) -> Vec<Check> {
    let health = match fetched {
        Ok(health) => health,
        Err(why) => return skipped(why),
    };
    let faults = match &health.faults.last {
        Some(last) if health.faults.count > 0 => (
            Verdict::Fail,
            format!(
                "{} request(s) failed in the last day; the newest: {} at {} ({})",
                health.faults.count, last.what, last.utc, last.error
            ),
        ),
        _ => (
            Verdict::Pass,
            "no request failed in the last day".to_owned(),
        ),
    };
    let doubled = match &health.doubled.last {
        Some(last) if health.doubled.count > 0 => (
            Verdict::Fail,
            format!(
                "{} minute(s) of the last fortnight show a mic's speech twice; the newest: {} at {}",
                health.doubled.count, last.source, last.start_utc
            ),
        ),
        _ => (
            Verdict::Pass,
            "no minute shown twice in the last fortnight".to_owned(),
        ),
    };
    vec![
        check("record", FAULTS, faults.0, faults.1, FAULTS_EXPECTED)
            .trend(health.faults.count as f64, "requests")
            .build(),
        check("record", DOUBLED, doubled.0, doubled.1, DOUBLED_EXPECTED)
            .trend(health.doubled.count as f64, "minutes")
            .build(),
    ]
}

fn skipped(why: &str) -> Vec<Check> {
    vec![
        check(
            "record",
            FAULTS,
            Verdict::Skip,
            why.to_owned(),
            FAULTS_EXPECTED,
        )
        .build(),
        check(
            "record",
            DOUBLED,
            Verdict::Skip,
            why.to_owned(),
            DOUBLED_EXPECTED,
        )
        .build(),
    ]
}

/// Both checks, when this Mac has no fleet to ask.
#[must_use]
pub fn unconfigured() -> Vec<Check> {
    skipped("no fleet configured — pass --fleet and set RECALL_SYNC_TOKEN")
}
