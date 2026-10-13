//! The server's background passes ([`audiocore::record_health::Pass`]): one
//! failing, or quiet, past its allowance fails. A pass only logs its own
//! failure, so without this one can fail for hours unseen.

use crate::check::{Check, Verdict, check};
use audiocore::record_health::{Pass, RecordHealth};
use chrono::{DateTime, Duration, Utc};

/// Silence or failure tolerated: three passes, and never under this.
const MIN_ALLOWANCE_MIN: i64 = 10;
/// One pass running longer than this is stuck. The longest is the view's
/// first, which renders every clip: about a minute on Isis.
const RUNNING_MAX_MIN: i64 = 60;

const EXPECTED: &str = "passes end, and end clean, within three intervals (at least 10 min); \
                        one run under an hour";

#[must_use]
pub fn pass_checks(fetched: &Result<RecordHealth, String>, now: DateTime<Utc>) -> Vec<Check> {
    let passes = match fetched {
        Ok(RecordHealth {
            passes: Some(passes),
            ..
        }) => passes,
        Ok(_) => return vec![skipped("the server is too old to report its passes")],
        Err(why) => return vec![skipped(why)],
    };
    passes.iter().map(|p| grade(p, now)).collect()
}

fn grade(pass: &Pass, now: DateTime<Utc>) -> Check {
    let (verdict, observed) = verdict(pass, now);
    check("record", label(&pass.name), verdict, observed, EXPECTED).build()
}

fn verdict(pass: &Pass, now: DateTime<Utc>) -> (Verdict, String) {
    if let Some(why) = &pass.off {
        return (Verdict::Skip, format!("switched off: {why}"));
    }
    let every = i64::try_from(pass.every_s).unwrap_or(i64::MAX);
    let allowance =
        Duration::seconds(every.saturating_mul(3)).max(Duration::minutes(MIN_ALLOWANCE_MIN));
    let minutes = |since: DateTime<Utc>| (now - since).num_minutes();
    let at = |raw: &str| audiocore::instant::parse_utc(raw);

    if let Some(failing) = &pass.failing {
        let since = at(&failing.since_utc).unwrap_or(now);
        let verdict = if now - since > allowance {
            Verdict::Fail
        } else {
            Verdict::Warn
        };
        return (
            verdict,
            format!("failing for {} min: {}", minutes(since), failing.error),
        );
    }
    if let Some(running) = pass.running_since_utc.as_deref().and_then(at) {
        let verdict = if now - running > Duration::minutes(RUNNING_MAX_MIN) {
            Verdict::Fail
        } else {
            Verdict::Pass
        };
        return (verdict, format!("running for {} min", minutes(running)));
    }
    match pass.last_end_utc.as_deref().and_then(at) {
        Some(end) if now - end > allowance => (
            Verdict::Fail,
            format!("quiet: the last pass ended {} min ago", minutes(end)),
        ),
        Some(end) => (
            Verdict::Pass,
            format!("the last pass ended {} min ago, clean", minutes(end)),
        ),
        None => {
            let since = at(&pass.since_utc).unwrap_or(now);
            if now - since > allowance {
                (
                    Verdict::Fail,
                    format!(
                        "no pass has run in the {} min since the server started",
                        minutes(since)
                    ),
                )
            } else {
                (Verdict::Pass, "starting".to_owned())
            }
        }
    }
}

fn label(name: &str) -> String {
    format!("background pass: {name}")
}

fn skipped(why: &str) -> Check {
    check(
        "record",
        "background passes",
        Verdict::Skip,
        why.to_owned(),
        EXPECTED,
    )
    .build()
}
