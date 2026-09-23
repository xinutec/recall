//! A microphone that delivers but hears nothing, while the mics beside it hear
//! a conversation.
//!
//! Such a source beats, streams and delivers on time, so every status check
//! passes; only the audio shows it. A lost microphone permission looks the same:
//! segments arrive on schedule holding digital silence, and no error surfaces.
//!
//! # Relative, never absolute
//!
//! A quiet house takes every microphone to zero together, and a switched-off
//! phone is not deaf. Only a disagreement between microphones over the same
//! minutes carries information, so:
//!
//! - a source that delivered nothing is absent from the comparison, never deaf
//!   in it (that is the delivery check's business).
//! - at least two peers must have heard a conversation; with one, the peer is
//!   as likely to be the odd one out.
//! - too little delivered audio says nothing rather than guessing.
//!
//! # Thresholds
//!
//! Not delicate: the observed gap was 43.3 s/min against 0.0, and any cut
//! between those behaves the same. A case landing between them is a new
//! finding, not a reason to nudge a constant.

use crate::check::{Check, Verdict, check};

/// Speech a source recorded, against how much audio it delivered.
///
/// A rate, not a per-minute bucket: recorders do not segment in phase (one
/// cuts at `:00`, others at `:57`), so clock-minute buckets would compare
/// barely overlapping audio. Speech per second delivered needs no alignment.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Heard {
    pub source: String,
    /// Seconds of audio this source delivered inside the window.
    pub delivered_s: f64,
    /// Seconds of detected speech within that audio.
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

    /// Speech per minute of delivered audio. Nothing delivered reads as zero
    /// rather than NaN; such a source is filtered out before this matters.
    #[must_use]
    pub fn per_minute(&self) -> f64 {
        if self.delivered_s <= 0.0 {
            return 0.0;
        }
        self.speech_s * 60.0 / self.delivered_s
    }

    /// Enough audio delivered for the rate to mean something.
    #[must_use]
    pub fn comparable(&self) -> bool {
        self.delivered_s >= MIN_DELIVERED_S
    }
}

/// How far back the deaf-microphone comparison looks.
///
/// A long window dilutes a conversation below [`CONVERSATION_PER_MIN`]: the
/// same mics measured 31-40 s/min over fifteen minutes of talk and 9-10 s/min
/// with eight hours of quiet folded in. Half an hour holds talk without
/// diluting it.
pub fn window() -> chrono::Duration {
    chrono::Duration::minutes(30)
}

/// Speech per minute at which a source is judged to have heard a conversation.
///
/// Measured: the three working mics sat at 43.3–54.2 s/min. Ten is far below the
/// quietest of them and far above a door closing.
pub const CONVERSATION_PER_MIN: f64 = 10.0;

/// Speech per minute at or below which a source has heard nothing.
///
/// Measured: the deaf one sat at exactly 0.0. Half a second allows for a single
/// detector twitch without softening the finding.
pub const DEAF_PER_MIN: f64 = 0.5;

/// Peers that must have heard a conversation before any source is accused.
pub const MIN_PEERS: usize = 2;

/// Audio a source must have delivered in the window before its rate is used:
/// one complete segment. It only has to show the source listened long enough
/// to hear something; peer agreement ([`MIN_PEERS`], [`CONVERSATION_PER_MIN`])
/// is the real protection.
///
/// ⚠ 55 and not 60: phones close segments at 59.993 s, so a floor at the
/// nominal length excludes every phone. Half a segment is still refused.
pub const MIN_DELIVERED_S: f64 = 55.0;

/// Stable across runs: the culprit belongs in `observed`, or the trend restarts
/// whenever a different microphone fails.
const LABEL: &str = "no microphone is deaf while the others hear speech";
const EXPECTED: &str = "every delivering source hears what its peers hear";

/// Name the sources that delivered audio containing no speech while at least
/// [`MIN_PEERS`] others heard a conversation over the same minutes.
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

/// The fleetwatch check; `Skip` where the comparison cannot speak.
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

/// Ask the fleet (`GET /sync/heard`) what each device source delivered and
/// heard over `window` to `now`. The fleet holds every recorder's audio and
/// speech measurement, including microphones that never pass through this Mac.
///
/// # Errors
/// The message is meant to be read in a skip line, so it names what failed.
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

/// [`deaf_check`] on whatever the fleet said; a fleet that could not be asked
/// SKIPS naming why, since the delivery checks are what go red for a down link.
#[must_use]
pub fn deaf_check_from(fetched: &Result<Vec<Heard>, String>) -> Check {
    match fetched {
        Ok(heard) => deaf_check(heard),
        Err(why) => skipped(why),
    }
}

/// The check when there is no fleet to ask.
#[must_use]
pub fn unconfigured() -> Check {
    skipped("no fleet configured — pass --fleet and set RECALL_SYNC_TOKEN")
}

fn skipped(why: &str) -> Check {
    check("archive", LABEL, Verdict::Skip, why.to_owned(), EXPECTED).build()
}
