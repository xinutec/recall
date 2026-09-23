//! The instant feed's health, read from the fleet and graded here.
//!
//! `org.xinutec.recall-live` runs beside this doctor but keeps nothing locally:
//! it pushes each turn, so the only record of what it produced is the fleet's.
//!
//! The fleet measures, this grades: every threshold and window is named here
//! and sent with the request, so there is one grader.
//!
//! An unreachable fleet skips, naming the network, and never fails: the
//! delivery checks already go red when the link is down.

use crate::capture::{self, WindowAudio};
use crate::check::{Check, Verdict, check};
use chrono::{DateTime, Duration, Utc};

/// Below this the sample is not a distribution and gets no verdict. A check
/// that grades three turns reports noise as a regression.
const MIN_LAG_SAMPLES: usize = 10;

/// Where the fleet is, and the credential for it.
///
/// The token is the one the Mac holds for every `/sync/*` read;
/// `doctorWrapper` sources `~/.config/recall/env` to provide it.
pub struct Fleet {
    pub url: String,
    pub token: String,
}

impl Fleet {
    /// `None` without a fleet URL or token: the checks then skip rather than
    /// guess a default address.
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
/// Short: this runs in the doctor's parent, outside the bounded child, so a
/// hung read would stall the whole report.
const TIMEOUT_S: u64 = 10;

/// A bounded, authenticated GET of `path` on the fleet.
///
/// ⚠ Callers add parameters with `.query`, not a formatted URL: an RFC3339
/// stamp ends in `+00:00`, and a raw `+` arrives as a space.
pub fn get(fleet: &Fleet, path: &str) -> ureq::Request {
    ureq::get(&format!("{}{path}", fleet.url))
        .set("Authorization", &format!("Bearer {}", fleet.token))
        .timeout(std::time::Duration::from_secs(TIMEOUT_S))
}

/// A failed fleet request, as a skip line reads it.
#[must_use]
pub fn describe(err: ureq::Error) -> String {
    match err {
        ureq::Error::Status(code, _) => format!("the fleet answered {code}"),
        ureq::Error::Transport(t) => format!("cannot reach the fleet ({t})"),
    }
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
    get(fleet, "/sync/live/health")
        .query(
            "lag_since",
            &audiocore::instant::python_isoformat_utc(now - lag_window),
        )
        .query(
            "window_since",
            &audiocore::instant::python_isoformat_utc(now - capture::live_quiet()),
        )
        .query(
            "window_until",
            &audiocore::instant::python_isoformat_utc(now),
        )
        .call()
        .map_err(describe)?
        .into_json::<LiveHealth>()
        .map_err(|e| format!("the fleet's answer did not parse ({e})"))
}

/// The two live checks, from whatever the fleet said.
///
/// Both skip together when the fleet cannot be asked, so neither reads as fine
/// by omission.
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
    // The sample floor is a grading rule, so it is applied here, not on the
    // fleet.
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
                .and_then(audiocore::instant::parse_utc),
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

/// The skip says the fleet was unreachable, not that the house was quiet.
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
