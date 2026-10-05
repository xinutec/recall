//! The instant feed's health. recall-live keeps nothing locally, so the server
//! measures; every threshold and window is set here and sent with the request.
//!
//! An unreachable server skips rather than fails: the delivery checks already
//! go red when the link is down.

use crate::capture::{self, WindowAudio};
use crate::check::{Check, Verdict, check};
use chrono::{DateTime, Duration, Utc};

/// Fewer turns than this get no lag verdict: the median would be noise.
const MIN_LAG_SAMPLES: usize = 10;

/// The server, and the sync token (`RECALL_SYNC_TOKEN`, from
/// `~/.config/recall/env` via `doctorWrapper`).
pub struct Fleet {
    pub url: String,
    pub token: String,
}

impl Fleet {
    /// `None` without a URL or token; the checks then skip.
    #[must_use]
    pub fn new(url: Option<&str>, token: Option<&str>) -> Option<Self> {
        Some(Self {
            url: url?.trim_end_matches('/').to_owned(),
            token: token.filter(|t| !t.is_empty())?.to_owned(),
        })
    }
}

/// `GET /sync/live/health`.
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

/// Short: these reads run in the reporting process, so a hung one would stall
/// the report.
const TIMEOUT_S: u64 = 10;

/// An authenticated GET with a timeout. Add parameters with `.query`: a raw
/// `+` in a formatted URL (`+00:00`) arrives as a space.
pub fn get(fleet: &Fleet, path: &str) -> ureq::Request {
    ureq::get(&format!("{}{path}", fleet.url))
        .set("Authorization", &format!("Bearer {}", fleet.token))
        .timeout(std::time::Duration::from_secs(TIMEOUT_S))
}

/// A failed request, worded for a skip line.
#[must_use]
pub fn describe(err: ureq::Error) -> String {
    match err {
        ureq::Error::Status(code, _) => format!("the fleet answered {code}"),
        ureq::Error::Transport(t) => format!("cannot reach the fleet ({t})"),
    }
}

/// # Errors
/// A message for a skip line, naming what failed.
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

/// The two live checks; both skip if the server could not be asked.
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

#[must_use]
pub fn unconfigured() -> Vec<Check> {
    let why = "no fleet configured — pass --fleet and set RECALL_SYNC_TOKEN";
    vec![
        skip("live delivery lag", why),
        skip("live transcription", why),
    ]
}
