//! A microphone that delivers but hears nothing while the mics beside it hear
//! a conversation. Every status check passes for it; a lost microphone
//! permission looks the same, digital silence delivered on schedule.
//!
//! Only disagreement between microphones over the same minutes means anything,
//! since a quiet house silences them all. So a source that delivered nothing
//! is left out (the delivery check covers it), at least two peers must have
//! heard a conversation, and too little audio gives no verdict.
//!
//! The thresholds are not delicate: the observed gap was 43.3 s/min against
//! 0.0. A case between them is a new finding, not a reason to tune.

use crate::check::{Check, Verdict, check};

/// Speech a source recorded against the audio it delivered. A rate, not
/// clock-minute buckets: recorders cut segments out of phase.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Heard {
    pub source: String,
    pub delivered_s: f64,
    pub speech_s: f64,
}

impl Heard {
    #[must_use]
    pub fn new(source: &str, delivered_s: f64, speech_s: f64) -> Self {
        Self {
            source: source.to_owned(),
            delivered_s,
            speech_s,
        }
    }

    /// Speech per minute of delivered audio; zero if nothing was delivered.
    #[must_use]
    pub fn per_minute(&self) -> f64 {
        if self.delivered_s <= 0.0 {
            return 0.0;
        }
        self.speech_s * 60.0 / self.delivered_s
    }

    #[must_use]
    pub fn comparable(&self) -> bool {
        self.delivered_s >= MIN_DELIVERED_S
    }
}

/// How far back the comparison looks. Longer dilutes a conversation below
/// [`CONVERSATION_PER_MIN`]: the same mics measured 31-40 s/min over fifteen
/// minutes of talk, 9-10 s/min with eight quiet hours added.
pub fn window() -> chrono::Duration {
    chrono::Duration::minutes(30)
}

/// Speech seconds per minute at which a source heard a conversation. The
/// working mics measured 43.3-54.2; a door closing is far below ten.
pub const CONVERSATION_PER_MIN: f64 = 10.0;

/// Speech seconds per minute at or below which a source heard nothing. The
/// deaf one measured 0.0; this allows one detector twitch.
pub const DEAF_PER_MIN: f64 = 0.5;

/// Peers that must have heard a conversation before any source is named.
pub const MIN_PEERS: usize = 2;

/// Audio a source must have delivered for its rate to count: one segment.
/// 55, not 60, because phones close segments at 59.993 s.
pub const MIN_DELIVERED_S: f64 = 55.0;

/// Names no source, so the trend survives a different mic failing.
const LABEL: &str = "no microphone is deaf while the others hear speech";
const EXPECTED: &str = "every delivering source hears what its peers hear";

/// The sources that delivered audio with no speech while at least
/// [`MIN_PEERS`] others heard a conversation.
#[must_use]
pub fn deaf_sources(heard: &[Heard]) -> Vec<String> {
    let talking = heard
        .iter()
        .filter(|h| h.per_minute() >= CONVERSATION_PER_MIN)
        .count();
    if talking < MIN_PEERS {
        return Vec::new();
    }
    heard
        .iter()
        .filter(|h| h.comparable() && h.per_minute() <= DEAF_PER_MIN)
        .map(|h| h.source.clone())
        .collect()
}

#[must_use]
pub fn deaf_check(heard: &[Heard]) -> Check {
    let usable: Vec<&Heard> = heard.iter().filter(|h| h.comparable()).collect();
    let talking = usable
        .iter()
        .filter(|h| h.per_minute() >= CONVERSATION_PER_MIN)
        .count();

    let (verdict, observed) = if usable.len() < MIN_PEERS + 1 {
        (
            Verdict::Skip,
            format!(
                "only {} source(s) delivered enough audio to compare",
                usable.len()
            ),
        )
    } else if talking < MIN_PEERS {
        (
            Verdict::Skip,
            "no conversation in the window — a quiet house says nothing about a microphone"
                .to_owned(),
        )
    } else {
        let deaf = deaf_sources(heard);
        if deaf.is_empty() {
            (
                Verdict::Pass,
                format!("{talking} source(s) heard the same conversation; none was silent"),
            )
        } else {
            (
                Verdict::Warn,
                format!(
                    "{} delivered audio with no speech while {talking} other(s) heard a \
                     conversation in the same minutes — check the mic's input, gain and \
                     permission before the room",
                    deaf.join(", ")
                ),
            )
        }
    };

    let deaf_count = deaf_sources(heard).len();
    check("archive", LABEL, verdict, observed, EXPECTED)
        .trend(deaf_count as f64, "sources")
        .build()
}

/// What each recorder delivered and heard in the `window` before `now`, from
/// the server, which has every mic including those not on this Mac.
///
/// # Errors
/// A message for a skip line, naming what failed.
pub fn fetch(
    fleet: &crate::live::Fleet,
    now: chrono::DateTime<chrono::Utc>,
    window: chrono::Duration,
) -> Result<Vec<Heard>, String> {
    crate::live::get(fleet, "/sync/heard")
        .query(
            "since",
            &audiocore::instant::python_isoformat_utc(now - window),
        )
        .query("until", &audiocore::instant::python_isoformat_utc(now))
        .call()
        .map_err(crate::live::describe)?
        .into_json::<Vec<Heard>>()
        .map_err(|e| format!("the fleet's answer did not parse ({e})"))
}

#[must_use]
pub fn deaf_check_from(fetched: &Result<Vec<Heard>, String>) -> Check {
    match fetched {
        Ok(heard) => deaf_check(heard),
        Err(why) => skipped(why),
    }
}

#[must_use]
pub fn unconfigured() -> Check {
    skipped("no fleet configured — pass --fleet and set RECALL_SYNC_TOKEN")
}

fn skipped(why: &str) -> Check {
    check("archive", LABEL, Verdict::Skip, why.to_owned(), EXPECTED).build()
}
