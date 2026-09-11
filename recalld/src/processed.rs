//! A microphone whose own noise suppression has destroyed the recording —
//! while every floor-based metric scores it BEST IN THE ROOM.
//!
//! ⚠ **The case is #1526, and geb on 2026-09-04 is the proof.** The best
//! hardware in the house, a speaker sitting beside it, and by ear: a voice with
//! no words. Phone-side NS gated the inter-speech frames to near-zero and
//! scrubbed off-voice spectrum during speech too. Nothing downstream noticed,
//! because everything downstream measures the FLOOR — and a scrubbed floor is
//! indistinguishable from an excellent one. It scored 52 dB "SNR", twice, under
//! two different floor definitions, and won per-bin selection outright.
//!
//! # The discriminator, measured rather than chosen
//!
//! A real microphone in a real room sits close above its own noise. Over the
//! 2026-09-04 archive (`segment_levels`, speech quantile 0.9 against floor
//! quantile 0.1):
//!
//! ```text
//!     source    segments   MEDIAN gap
//!     usb           489       14.3
//!     iphone11      417       15.9
//!     pixel5        189       21.7
//!     pixel9        123       24.7
//!     geb           148       70.9   <- the defect
//! ```
//!
//! Four genuine microphones span 14.3 to 24.7 dB. geb sits 70.9 dB above its own
//! floor. Only a gate produces that: silence between words is not quiet, it is
//! EMPTY.
//!
//! ⚠ **Read the MEDIAN of per-segment gaps, not a gap between averages.** An
//! earlier draft of this comment computed it the second way and reported 13.1 to
//! 20.4 for the real mics and 55.7 for geb — same conclusion, wrong numbers, and
//! a range narrow enough that the threshold below would have looked tuned. The
//! per-segment spread is what the check actually consumes, so it is what is
//! quoted here.
//!
//! ⚠ **The threshold is not delicate and must not be tuned.** Any cut between
//! 24.7 and 70.9 behaves identically on the real data — a 46 dB gap. A source
//! landing inside it is a NEW finding and wants reading, not a nudge here, the
//! same rule `deaf` states for the same reason.
//!
//! # Why this cannot be folded into `deaf`
//!
//! `deaf` asks whether a source heard the conversation. This source DID: geb's
//! speech level is the second highest in the room. The audio arrives, carries
//! speech, transcribes into plausible text, and is wrong. Absence of speech and
//! destruction of speech are different faults with opposite signatures.
//!
//! # Why it is per-source and not relative to peers
//!
//! Unlike `deaf`, this needs no agreement between microphones: the gap is a
//! property of ONE stream against ITSELF. A house where every mic was denoised
//! would defeat a peer comparison and is exactly the case worth catching.

/// One segment's level evidence: the dB of its envelope's speech quantile
/// against its floor quantile — recalld's `segment_levels`, recomputed here from
/// the envelope the Mac already stores.
#[derive(Debug, Clone, PartialEq)]
pub struct Levels {
    pub source: String,
    pub speech_db: f32,
    pub floor_db: f32,
}

impl Levels {
    #[must_use]
    pub fn gap_db(&self) -> f32 {
        self.speech_db - self.floor_db
    }

    /// Whether this segment says anything about the microphone.
    ///
    /// A segment with no speech in it has no speech level to compare, and its
    /// gap is noise about noise. `-75 dB` is below every real speech level in
    /// the table above (the quietest, pixel5, is -77.0 on a day it was faulty —
    /// so this deliberately admits a weak mic and excludes silence).
    #[must_use]
    pub fn carries_speech(&self) -> bool {
        self.speech_db > MIN_SPEECH_DB && self.floor_db.is_finite()
    }
}

/// Above this, the silence between words is not quiet — it is empty.
pub const PROCESSED_GAP_DB: f32 = 35.0;
/// A speech level below this is silence, not a quiet talker.
pub const MIN_SPEECH_DB: f32 = -75.0;
/// Fewer segments than this says nothing rather than guessing.
pub const MIN_SEGMENTS: usize = 10;

fn median(mut values: Vec<f32>) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(values[values.len() / 2])
}

/// The median speech-to-floor gap per source, over segments that carry speech.
///
/// Median rather than mean: one genuinely silent minute that slipped the speech
/// filter would drag a mean, and the question is what this microphone does
/// USUALLY.
#[must_use]
pub fn gaps(levels: &[Levels]) -> Vec<(String, f32, usize)> {
    let mut sources: Vec<&str> = levels.iter().map(|l| l.source.as_str()).collect();
    sources.sort_unstable();
    sources.dedup();
    let mut out = Vec::new();
    for source in sources {
        let usable: Vec<f32> = levels
            .iter()
            .filter(|l| l.source == source && l.carries_speech())
            .map(Levels::gap_db)
            .collect();
        let n = usable.len();
        if let Some(m) = median(usable) {
            out.push((source.to_owned(), m, n));
        }
    }
    out
}

/// Sources delivering a processed stream.
#[must_use]
pub fn processed_sources(levels: &[Levels]) -> Vec<String> {
    gaps(levels)
        .into_iter()
        .filter(|(_, gap, n)| *n >= MIN_SEGMENTS && *gap >= PROCESSED_GAP_DB)
        .map(|(source, _, _)| source)
        .collect()
}

/// Every flagged source with the evidence against it: `(source, median gap dB,
/// segments considered)`.
///
/// Returned rather than rendered: the verdict surface is `doctor`'s, and this
/// crate has no business owning how a warning reads. What it owns is the
/// measurement.
#[must_use]
pub fn processed_with_evidence(levels: &[Levels]) -> Vec<(String, f32, usize)> {
    gaps(levels)
        .into_iter()
        .filter(|(_, gap, n)| *n >= MIN_SEGMENTS && *gap >= PROCESSED_GAP_DB)
        .collect()
}

/// Read one window's level evidence from `segment_levels`.
///
/// ⚠ **This is recalld's own table, and it has to be.** The obvious reader —
/// recompute from `audio_segments.envelope` on the Mac — cannot work: measured
/// 2026-09-11, that column is WRITE-DEAD. Four of the 14,595 segments since
/// 2026-07-12 carry one, and the last real envelope was written
/// 2026-07-12T15:50:27, when D2 moved level measurement into the scanner here
/// and the column was left behind. `mean_volume` went with it: zero of
/// September's segments have one.
///
/// The room stream is excluded: it is BUILT from whichever microphone won each
/// minute, so it inherits their levels and is not a device to diagnose.
///
/// # Errors
/// If the query fails.
pub fn levels_between(
    conn: &rusqlite::Connection,
    since: &str,
    until: &str,
) -> rusqlite::Result<Vec<Levels>> {
    let mut stmt = conn.prepare(
        "SELECT l.source, l.speech_db, l.floor_db
         FROM segment_levels l JOIN segments s ON s.filename = l.filename
         WHERE s.start_utc >= ?1 AND s.start_utc < ?2 AND l.source != ?3",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![since, until, crate::room::ROOM_SOURCE],
        |row| {
            Ok(Levels {
                source: row.get(0)?,
                speech_db: row.get::<_, f64>(1)? as f32,
                floor_db: row.get::<_, f64>(2)? as f32,
            })
        },
    )?;
    rows.collect()
}
