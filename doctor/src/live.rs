//! The instant feed's health, read from the fleet and graded here.
//!
//! ⚠ **The subject is a Mac agent whose output lands elsewhere.**
//! `org.xinutec.recall-live` runs beside this doctor and keeps no store — the
//! push IS the write — so the only record of what it produced is the fleet's.
//! Reading the Mac's archive for it, which is what these checks did until
//! 2026-09-21, grades a database the tier stopped writing to: both skipped
//! forever, and a blind check reads as a quiet house (#1671).
//!
//! ⚠ **The fleet measures, this grades.** Every threshold and every window is
//! named here and sent with the request, so there is one grader rather than two
//! that could disagree about what "behind" means.
//!
//! ⚠ **An unreachable fleet SKIPS naming the network — never fails.** The Mac's
//! own delivery checks are what go red when the link is down, so an outage is
//! reported once, by the check whose subject it is.

use crate::capture::{self, WindowAudio};
use crate::check::{Check, Verdict, check};
use chrono::{DateTime, Duration, SecondsFormat, Utc};

/// Below this the sample is not a distribution and gets no verdict. A check
/// that grades three turns reports noise as a regression.
const MIN_LAG_SAMPLES: usize = 10;

/// Where the fleet is, and the credential for it.
///
/// ⓘ The token is the one the Mac already holds for every other `/sync/*` read
/// — `doctorWrapper` sources `~/.config/recall/env` for exactly this reason.
pub struct Fleet {
    pub url: String,
    pub token: String,
}

impl Fleet {
    /// `None` when this Mac is not half of the Isis pair, or has no token: the
    /// checks then skip saying which it was, rather than inventing a default
    /// address and reporting that nothing answered there.
    #[must_use]
    pub fn new(url: Option<&str>, token: Option<&str>) -> Option<Self> {
        Some(Self {
            url: url?.trim_end_matches('/').to_owned(),
            token: token.filter(|t| !t.is_empty())?.to_owned(),
        })
    }
}

/// What `GET /sync/live/health` answers with.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveHealth {
    pub lag_median_s: Option<f64>,
    pub lag_samples: usize,
    pub newest_turn_utc: Option<String>,
    pub delivered_s: f64,
    pub scanned_s: f64,
    pub speech_s: f64,
}

/// How long to wait on the fleet before calling it unreachable.
///
/// Short on purpose: this runs in the doctor's PARENT, which has no bounded
/// child around it, so a hung read would stall the whole report — including the
/// checks that would have said the archive was fine.
const TIMEOUT_S: u64 = 10;

fn stamp(when: DateTime<Utc>) -> String {
    when.to_rfc3339_opts(SecondsFormat::Micros, false)
}

/// Ask the fleet for the numbers, over the windows this grader uses.
///
/// # Errors
/// The message is meant to be read in a skip line, so it names what failed
/// rather than carrying a type.
pub fn fetch(
    fleet: &Fleet,
    now: DateTime<Utc>,
    lag_window: Duration,
) -> Result<LiveHealth, String> {
    // ⚠ `.query` rather than a formatted URL: an RFC3339 stamp ends in `+00:00`
    // and a raw `+` arrives at the other end as a SPACE, so the fleet would
    // parse a different instant than the one asked about — quietly, and only
    // for windows, which is the hardest kind of wrong to notice.
    ureq::get(&format!("{}/sync/live/health", fleet.url))
        .set("Authorization", &format!("Bearer {}", fleet.token))
        .timeout(std::time::Duration::from_secs(TIMEOUT_S))
        .query("lag_since", &stamp(now - lag_window))
        .query("window_since", &stamp(now - capture::live_quiet()))
        .query("window_until", &stamp(now))
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(code, _) => format!("the fleet answered {code}"),
            ureq::Error::Transport(t) => format!("cannot reach the fleet ({t})"),
        })?
        .into_json::<LiveHealth>()
        .map_err(|e| format!("the fleet's answer did not parse ({e})"))
}

/// The two live checks, from whatever the fleet said.
///
/// ⚠ Both skip together when the fleet cannot be asked. Reporting one and not
/// the other would leave a reader to infer that the silent one was fine.
#[must_use]
pub fn live_checks(
    fetched: &Result<LiveHealth, String>,
    now: DateTime<Utc>,
    paused_until: Option<DateTime<Utc>>,
) -> Vec<Check> {
    let health = match fetched {
        Ok(health) => health,
        Err(why) => {
            return vec![
                skip("live delivery lag", why),
                skip("live transcription", why),
            ];
        }
    };
    // ⚠ The sample floor is applied HERE rather than on the fleet: it is a
    // grading rule, and a measurement that hid its own sample size could not be
    // graded by any other one.
    let median = health
        .lag_median_s
        .filter(|_| health.lag_samples >= MIN_LAG_SAMPLES);
    let too_few = format!(
        "{} live turn(s) in the window — too few to call a median",
        health.lag_samples
    );
    vec![
        capture::live_lag_check(median, capture::live_lag_slow(), &too_few),
        capture::live_check(
            health
                .newest_turn_utc
                .as_deref()
                .and_then(crate::instant::parse),
            now,
            paused_until,
            capture::live_quiet(),
            WindowAudio {
                delivered_s: health.delivered_s,
                scanned_s: health.scanned_s,
                speech_s: health.speech_s,
            },
        ),
    ]
}

/// ⚠ The skip must say the fleet was unreachable, not that the house was quiet.
/// A skip whose reason is wrong is how a blind check gets trusted.
fn skip(label: &'static str, why: &str) -> Check {
    check(
        "capture",
        label,
        Verdict::Skip,
        why.to_owned(),
        "the fleet answers for the live tier",
    )
    .build()
}

/// Both checks, when this Mac has no fleet to ask.
#[must_use]
pub fn unconfigured() -> Vec<Check> {
    let why = "no fleet configured — pass --fleet and set RECALL_SYNC_TOKEN";
    vec![
        skip("live delivery lag", why),
        skip("live transcription", why),
    ]
}
