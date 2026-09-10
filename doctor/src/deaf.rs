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
#[derive(Debug, Clone, PartialEq)]
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

/// Audio a source must have delivered in the window before its rate is used.
///
/// ⚠ The residual assumption, stated because it is not eliminated: the recorders
/// run continuously while capture is active, so delivering three minutes inside
/// one short window means they heard the same stretch of room. A source that
/// delivered ONLY during a genuinely quiet patch would read as deaf. That is why
/// the verdict is a WARN naming what to check, not a FAIL.
pub const MIN_DELIVERED_S: f64 = 180.0;

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
    check(
        "archive",
        // ⚠ Stable across runs: the culprit belongs in `observed`, never here,
        // or the trend restarts whenever a different microphone fails.
        "no microphone is deaf while the others hear speech",
        verdict,
        observed,
        "every delivering source hears what its peers hear",
    )
    .trend(deaf_count as f64, "sources")
    .build()
}

/// Read what each device source delivered and heard since `since`.
///
/// ⚠ Only segments the speech scanner has MEASURED (`speech_s IS NOT NULL`)
/// count, on both sides of the ratio. An unmeasured segment is unknown, not
/// silent, and counting its duration as delivered while its speech reads zero
/// would manufacture a deaf source out of a scanner that has not caught up.
pub fn heard_between(
    conn: &rusqlite::Connection,
    sources: &[(String, crate::source::SourceKind)],
    since: chrono::DateTime<chrono::Utc>,
    until: chrono::DateTime<chrono::Utc>,
) -> rusqlite::Result<Vec<Heard>> {
    let mut out = Vec::new();
    for (source, kind) in sources {
        if !kind.is_device() {
            continue;
        }
        let mut stmt = conn.prepare(
            "SELECT start_utc, end_utc, speech_s FROM audio_segments
             WHERE source_id = ?1 AND start_utc >= ?2 AND start_utc < ?3
               AND speech_s IS NOT NULL",
        )?;
        let rows = stmt
            .query_map(
                rusqlite::params![
                    source,
                    crate::archive::python_iso(since),
                    crate::archive::python_iso(until)
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, f64>(2)?,
                    ))
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut delivered = 0.0;
        let mut speech = 0.0;
        for (start, end, speech_s) in rows {
            let (Some(start), Some(end)) =
                (crate::instant::parse(&start), crate::instant::parse(&end))
            else {
                continue;
            };
            let span = (end - start).num_milliseconds() as f64 / 1000.0;
            if span <= 0.0 {
                continue;
            }
            delivered += span;
            speech += speech_s;
        }
        if delivered > 0.0 {
            out.push(Heard::new(source, delivered, speech));
        }
    }
    out.sort_by(|a, b| a.source.cmp(&b.source));
    Ok(out)
}
