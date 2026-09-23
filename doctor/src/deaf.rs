//! A microphone that DELIVERS but hears nothing, while the mics beside it hear
//! a conversation.
//!
//! ⚠ **The gap this fills is named in #1485.** On 2026-09-08 pixel5 delivered
//! five 60-second segments containing 0.0 s of speech while usb, iphone11 and
//! oneplus6t heard 54.2, 53.8 and 43.3 s per minute in the SAME minutes. It beat,
//! it streamed, it delivered on time — so mic-alive passed, liveness passed and
//! the delivery check passed. Every signal the fleet had said the phone was fine.
//! Nothing measured what was IN the audio.
//!
//! ⚠ **The same signature has a second cause, and it is why this was written on
//! 2026-09-10 rather than later**: a microphone permission lost on the Mac. The
//! recorder logs `capture: listening`, segments arrive on schedule, every check
//! goes green, and the files hold digital silence. A denial never surfaces as an
//! error, so a check that reads status can never see it — only one that reads the
//! audio can.
//!
//! # Why the rule is relative and never absolute
//!
//! A quiet house takes every microphone to zero together, and that is not a
//! fault. An absolute floor is what once filed a phone Pippijn had deliberately
//! switched off as "a microphone in your house is missing three quarters of what
//! is said" — the measurement was right and the conclusion was wrong. Only a
//! DISAGREEMENT between microphones over the same minutes carries information.
//!
//! Three consequences, each with a test:
//!
//! - a source that delivered NOTHING is absent from the comparison, never deaf
//!   in it. A switched-off phone is the delivery check's business.
//! - at least two peers must have heard a conversation. With one, the peer that
//!   heard it is as likely to be the odd one out as the source that did not.
//! - too few shared minutes says nothing rather than guessing.
//!
//! # On the thresholds
//!
//! ⚠ They are deliberately not delicate, and the measurement is why: the gap
//! observed was **43.3 s/min against 0.0**. Any cut between those two behaves
//! identically on the real data, so these numbers are a statement about what a
//! conversation sounds like, not a tuned parameter. If a future case lands
//! *between* them, that is a new finding and wants reading, not a nudge here.

use crate::check::{Check, Verdict, check};

/// Speech a source recorded, against how much audio it delivered.
///
/// ⚠ **A RATE, not a per-minute bucket, and the archive is why.** The recorders
/// do not segment in phase: measured 2026-09-08, iphone11 cut its minute at
/// `:00` while usb, oneplus6t and pixel5 cut theirs at `:57`–`:58`. Bucketing by
/// clock minute would have compared segments overlapping by two seconds and
/// called it the same minute. Speech per second DELIVERED needs no alignment.
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
/// ⚠ **A long window hides the thing it is looking for.** The comparison only
/// speaks when peers heard a CONVERSATION, and a rate averaged over a night of
/// sleep falls below that: measured on the real archive, the same microphones
/// read 31-40 s/min over the fifteen minutes of an actual conversation and
/// 9-10 s/min once eight hours of quiet were folded in. Half an hour is long
/// enough to contain talking and short enough not to dilute it away.
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

/// Audio a source must have delivered in the window before its rate is used —
/// one complete 60-second segment.
///
/// ⚠ **This was 180 s and it was wrong, shown by the case the check exists for.**
/// Measured 2026-09-10 20:35Z, Pippijn alone reading a known script: every mic
/// delivered ONE segment, four heard 21-25 s of him, pixel5 heard nothing and
/// produced no turns. The check SKIPPED — "only 0 source(s) delivered enough
/// audio to compare" — because sixty seconds is not a hundred and eighty.
///
/// What the floor guards against is a source that delivered only during a quiet
/// patch, so its zero says nothing about the microphone. That risk is absent
/// when several peers each heard twenty seconds over the SAME minute: the
/// protection that matters is peer agreement, and it is enforced separately by
/// [`MIN_PEERS`] and [`CONVERSATION_PER_MIN`]. This floor only has to establish
/// that the source was listening long enough to have heard something.
///
/// ⚠ **55 and not 60, because a real segment is not 60 seconds.** Measured on the
/// same archive: the phones close at **59.993 s** and only the Mac's own capture
/// hits 60.000. A floor set at the nominal length excluded all four phones and
/// left the check saying "only 1 source delivered enough audio" — the SECOND
/// time this bound hid the case it exists for, and the first fixture missed it
/// because it used the nominal 60.0 rather than the measured 59.993.
///
/// ⚠ It still refuses half a segment — a source cut off by a pause or starting
/// mid-minute is genuinely too little to convict on, and a test pins that.
pub const MIN_DELIVERED_S: f64 = 55.0;

/// ⚠ Stable across runs: the culprit belongs in `observed`, never here, or the
/// trend restarts whenever a different microphone fails.
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

/// The fleetwatch check. `Skip` where the comparison cannot speak — that is an
/// absence of evidence, and ranks with `Pass` rather than dragging a summary up.
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

/// Ask the fleet what each device source delivered and heard over `window` to
/// `now`. The fleet holds every recorder's audio and its speech measurement, so
/// the comparison covers the microphones that never pass through this Mac too.
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
