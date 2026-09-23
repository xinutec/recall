//! A microphone whose own noise suppression has destroyed the recording, while
//! every floor-based metric scores it best in the room.
//!
//! Phone-side noise suppression can gate the frames between words to near-zero
//! and scrub off-voice spectrum during speech: by ear, a voice with no words.
//! A scrubbed floor is indistinguishable from an excellent one, so floor-based
//! SNR scores such a source highest.
//!
//! # The discriminator, measured rather than chosen
//!
//! A real microphone in a real room sits close above its own noise. Over one
//! day's archive (`segment_levels`, speech quantile 0.9 against floor quantile
//! 0.1):
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
//! Four genuine microphones span 14.3 to 24.7 dB; the gated one sits 70.9 dB
//! above its floor. Only a gate produces that. The figures are medians of
//! per-segment gaps, which is what the check consumes.
//!
//! ⚠ The threshold is not delicate and must not be tuned: any cut between 24.7
//! and 70.9 behaves identically on this data. A source landing inside that
//! range is a new finding to investigate, not a reason to move the cut.
//!
//! # Why this is not doctor's `deaf` check
//!
//! `deaf` asks whether a source heard the conversation. A gated source did: its
//! speech level can be among the highest in the room, and it transcribes into
//! plausible, wrong text. Absent speech and destroyed speech have opposite
//! signatures.
//!
//! # Why it is per-source and not relative to peers
//!
//! The gap is a property of one stream against itself, so it needs no
//! agreement between microphones. If every microphone were denoised, a peer
//! comparison would miss it; this would not.

/// One segment's level evidence: the dB of its envelope's speech quantile
/// against its floor quantile, as stored in `segment_levels`.
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
    /// A segment with no speech has no speech level to compare, and its gap is
    /// noise about noise. [`MIN_SPEECH_DB`] admits a weak microphone and
    /// excludes silence.
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
/// Median rather than mean: a silent minute that slipped the speech filter
/// would drag a mean, and the question is what the microphone usually does.
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
/// Returned rather than rendered: `doctor` owns how the warning reads.
#[must_use]
pub fn processed_with_evidence(levels: &[Levels]) -> Vec<(String, f32, usize)> {
    gaps(levels)
        .into_iter()
        .filter(|(_, gap, n)| *n >= MIN_SEGMENTS && *gap >= PROCESSED_GAP_DB)
        .collect()
}

/// Read one window's level evidence from `segment_levels`.
///
/// ⚠ Levels live only here, in recalld's own table, written by the scanner in
/// `levels.rs`. `audio_segments.envelope` and `mean_volume` are no longer
/// written.
///
/// The room stream is excluded: it is built from whichever microphone won each
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

/// This SOURCE's median speech-to-floor gap over its most recent `window`
/// segments, or `None` while it has too little history to mean anything.
///
/// The per-source read the room builder needs, beside [`gaps`]'s whole-fleet
/// one. `None` at fewer than [`MIN_SEGMENTS`]: a source that cannot be measured
/// is unmeasured, never assumed healthy and never condemned.
///
/// # Errors
/// If the database refuses.
pub fn source_gap(
    conn: &rusqlite::Connection,
    source: &str,
    window: u32,
) -> rusqlite::Result<Option<f32>> {
    let mut stmt = conn.prepare(
        "SELECT l.speech_db, l.floor_db FROM segment_levels l
         WHERE l.source = ?1
         ORDER BY l.filename DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![source, window], |row| {
        Ok(Levels {
            source: source.to_owned(),
            speech_db: row.get::<_, f64>(0)? as f32,
            floor_db: row.get::<_, f64>(1)? as f32,
        })
    })?;
    // Through `carries_speech` and `gap_db` rather than arithmetic here, so the
    // per-source answer cannot drift from the whole-fleet one.
    let usable: Vec<f32> = rows
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(Levels::carries_speech)
        .map(|l| l.gap_db())
        .collect();
    if usable.len() < MIN_SEGMENTS {
        return Ok(None);
    }
    Ok(median(usable))
}
