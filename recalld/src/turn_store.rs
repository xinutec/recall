//! Every write to `transcript_segments`, and the types that say what a row is.
//!
//! ⚠ No other module writes the table: `the_turn_table_has_one_writer` fails the
//! build otherwise. A writer that records its work in a way the readers do not
//! recognise is the bug this closes: a pass naming turns in place set only the
//! cluster, and the tier, read from the provenance, called them undiarized.

use audiocore::instant::Stamp;
use std::borrow::Cow;
use std::fmt;

use rusqlite::Connection;

/// `asr_model` of a turn a person wrote.
pub const HUMAN_MODEL: &str = "human";
/// `asr_model` of the provisional live pass.
pub const LIVE_MODEL: &str = "live";

/// How a row came to be. Stored as text in `provenance`; the spellings are
/// reversal keys (`DELETE ... WHERE provenance = ...`), so they never change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Provenance {
    /// The older pipeline recorded the model's name here.
    Model(String),
    /// The per-microphone transcription pass: `per-mic (runner)`.
    PerMic,
    /// The derived room stream: `room`.
    Room,
    /// A diarized refine before word alignment: `diarized (<by>)`.
    Diarized(Cow<'static, str>),
    /// A word-aligned diarized pass: `diarized-aligned (<by>)`.
    DiarizedAligned(Cow<'static, str>),
    /// A piece of a split diarized turn: `diarized-aligned split of #<id>`.
    DiarizedSplit(i64),
    /// A piece of a split undiarized turn: `split of #<id>`.
    Split(i64),
    /// A person's correction of a turn: `human correction of #<id>`.
    Correction(i64),
}

impl Provenance {
    /// Whether a speaker pass produced this row.
    pub const fn is_diarized(&self) -> bool {
        matches!(
            self,
            Self::Diarized(_) | Self::DiarizedAligned(_) | Self::DiarizedSplit(_)
        )
    }
}

impl fmt::Display for Provenance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Model(model) => f.write_str(model),
            Self::PerMic => f.write_str("per-mic (runner)"),
            Self::Room => f.write_str("room"),
            Self::Diarized(by) => write!(f, "diarized ({by})"),
            Self::DiarizedAligned(by) => write!(f, "diarized-aligned ({by})"),
            Self::DiarizedSplit(of) => write!(f, "diarized-aligned split of #{of}"),
            Self::Split(of) => write!(f, "split of #{of}"),
            Self::Correction(of) => write!(f, "human correction of #{of}"),
        }
    }
}

/// A stored provenance no writer produces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownProvenance(pub String);

impl fmt::Display for UnknownProvenance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown provenance {:?}", self.0)
    }
}

impl std::error::Error for UnknownProvenance {}

impl std::str::FromStr for Provenance {
    type Err = UnknownProvenance;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let id = |rest: &str| rest.parse::<i64>().ok();
        let within = |rest: &str| rest.strip_prefix('(')?.strip_suffix(')').map(str::to_owned);
        let parsed = match raw {
            "per-mic (runner)" => Some(Self::PerMic),
            "room" => Some(Self::Room),
            _ => {
                if let Some(rest) = raw.strip_prefix("human correction of #") {
                    id(rest).map(Self::Correction)
                } else if let Some(rest) = raw.strip_prefix("diarized-aligned split of #") {
                    id(rest).map(Self::DiarizedSplit)
                } else if let Some(rest) = raw.strip_prefix("split of #") {
                    id(rest).map(Self::Split)
                } else if let Some(rest) = raw.strip_prefix("diarized-aligned ") {
                    within(rest).map(|by| Self::DiarizedAligned(by.into()))
                } else if let Some(rest) = raw.strip_prefix("diarized ") {
                    within(rest).map(|by| Self::Diarized(by.into()))
                } else if raw.contains('/') && !raw.contains(char::is_whitespace) {
                    // A model id (`org/name`) or a path: the older pipeline's spelling.
                    Some(Self::Model(raw.to_owned()))
                } else {
                    None
                }
            }
        };
        parsed.ok_or_else(|| UnknownProvenance(raw.to_owned()))
    }
}

/// How much processing a turn has had: the tier the app shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, rename = "Tier")]
pub enum Stage {
    Live,
    Transcribed,
    Diarized,
    Corrected,
}

impl Stage {
    /// The one derivation. A pass that keeps the boundaries names turns in place:
    /// it records the cluster and leaves the provenance alone.
    pub fn of(asr_model: Option<&str>, provenance: Option<&Provenance>, cluster: bool) -> Self {
        match asr_model {
            Some(HUMAN_MODEL) => Self::Corrected,
            Some(LIVE_MODEL) => Self::Live,
            _ if cluster || provenance.is_some_and(Provenance::is_diarized) => Self::Diarized,
            _ => Self::Transcribed,
        }
    }
}

/// Why a pass hid a turn, as recorded in `hidden_reason`. Like provenance, a
/// reversal key. Reasons typed by hand during past repairs are not listed:
/// nothing writes them any more.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HiddenReason {
    /// The archive pass wrote the clip; the live guess steps aside.
    LiveReconciled,
    /// A room turn covers it.
    CoveredByRoom,
    /// A diarized pass replaced it: `diarized (<by>)`.
    DiarizedBy(Cow<'static, str>),
    /// A person split it: `split into pieces (<id>)`.
    SplitInto(i64),
    /// A person listened and nobody spoke: the model invented the words.
    NobodySpoke,
    /// The speech pass measured the clip silent (0 s). Written once, in bulk,
    /// for turns from before the queue waited for that measurement (#1782).
    SilentMinute,
}

impl fmt::Display for HiddenReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LiveReconciled => f.write_str("live-reconciled"),
            Self::CoveredByRoom => f.write_str("covered by the room stream"),
            Self::DiarizedBy(by) => write!(f, "diarized ({by})"),
            Self::SplitInto(id) => write!(f, "split into pieces ({id})"),
            Self::NobodySpoke => f.write_str("nobody spoke"),
            Self::SilentMinute => f.write_str("silent minute"),
        }
    }
}

/// SQL for "a person owns this turn": they wrote its words or named its
/// speaker. No pass hides such a turn or writes over its span.
/// `a_person_owns_what_the_predicate_says` ties it to [`is_human_owned`].
pub const HUMAN_OWNED: &str = "(asr_model = 'human' OR speaker_label IS NOT NULL)";

/// The same rule in Rust.
pub fn is_human_owned(asr_model: Option<&str>, speaker_label: Option<&str>) -> bool {
    asr_model == Some(HUMAN_MODEL) || speaker_label.is_some()
}

/// A span a pass must leave alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Protected {
    pub start: chrono::DateTime<chrono::Utc>,
    pub end: chrono::DateTime<chrono::Utc>,
}

/// Every protected span overlapping `[from, to)`: corrections, and the current
/// turns a person owns. An unparseable stamp is skipped: a span at the epoch
/// would protect nothing.
///
/// # Errors
/// If the database refuses.
pub fn protected_between(
    conn: &Connection,
    from: chrono::DateTime<chrono::Utc>,
    to: chrono::DateTime<chrono::Utc>,
) -> rusqlite::Result<Vec<Protected>> {
    let sql = format!(
        "SELECT start_utc, end_utc FROM corrections WHERE start_utc < ?2 AND end_utc > ?1
         UNION ALL
         SELECT start_utc, end_utc FROM transcript_segments
          WHERE start_utc < ?2 AND end_utc > ?1
            AND superseded_by IS NULL AND hidden_reason IS NULL AND {HUMAN_OWNED}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params![
            audiocore::instant::python_isoformat_utc(from),
            audiocore::instant::python_isoformat_utc(to)
        ],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
    )?;
    let parse = |raw: &str| {
        chrono::DateTime::parse_from_rfc3339(raw)
            .ok()
            .map(|t| t.with_timezone(&chrono::Utc))
    };
    let mut out = Vec::new();
    for row in rows {
        let (start, end) = row?;
        if let (Some(start), Some(end)) = (parse(&start), parse(&end)) {
            out.push(Protected { start, end });
        }
    }
    Ok(out)
}

/// A turn to insert.
#[derive(Debug, Clone)]
pub struct NewTurn<'a> {
    pub audio_segment_id: Option<i64>,
    pub start_utc: &'a Stamp,
    pub end_utc: &'a Stamp,
    pub text: &'a str,
    pub language: Option<&'a str>,
    pub language_confidence: Option<f64>,
    pub asr_confidence: Option<f64>,
    pub asr_model: Option<&'a str>,
    pub speaker_label: Option<&'a str>,
    pub speaker_id: Option<i64>,
    pub speaker_cluster: Option<&'a str>,
    /// `None` only for a live turn.
    pub provenance: Option<Provenance>,
    pub word_timings: Option<&'a str>,
    pub created_utc: Option<&'a Stamp>,
}

impl<'a> NewTurn<'a> {
    /// A turn with only what every turn has; the rest are set by struct update.
    pub const fn at(start_utc: &'a Stamp, end_utc: &'a Stamp, text: &'a str) -> Self {
        Self {
            audio_segment_id: None,
            start_utc,
            end_utc,
            text,
            language: None,
            language_confidence: None,
            asr_confidence: None,
            asr_model: None,
            speaker_label: None,
            speaker_id: None,
            speaker_cluster: None,
            provenance: None,
            word_timings: None,
            created_utc: None,
        }
    }
}

/// Insert a turn and its search-index row, returning its id.
///
/// ⚠ `transcript_fts` is contentless FTS5 kept by the writer, not a trigger;
/// a turn missing from it is silently unsearchable. Hence one function.
///
/// # Errors
/// If the database refuses.
pub fn insert(conn: &Connection, turn: &NewTurn<'_>) -> rusqlite::Result<i64> {
    use rusqlite::types::Value;
    let text = |v: Option<&str>| v.map(|s| Value::Text(s.to_owned()));
    let provenance = turn.provenance.as_ref().map(ToString::to_string);
    // Only the columns this turn has: an absent one is NULL either way.
    let columns: Vec<(&str, Value)> = [
        (
            "audio_segment_id",
            turn.audio_segment_id.map(Value::Integer),
        ),
        ("start_utc", Some(Value::Text(turn.start_utc.to_string()))),
        ("end_utc", Some(Value::Text(turn.end_utc.to_string()))),
        ("text", text(Some(turn.text))),
        ("language", text(turn.language)),
        (
            "language_confidence",
            turn.language_confidence.map(Value::Real),
        ),
        ("asr_confidence", turn.asr_confidence.map(Value::Real)),
        ("asr_model", text(turn.asr_model)),
        ("speaker_label", text(turn.speaker_label)),
        ("speaker_id", turn.speaker_id.map(Value::Integer)),
        ("speaker_cluster", text(turn.speaker_cluster)),
        ("provenance", provenance.map(Value::Text)),
        ("word_timings", text(turn.word_timings)),
        (
            "created_utc",
            turn.created_utc.map(|t| Value::Text(t.to_string())),
        ),
    ]
    .into_iter()
    .filter_map(|(name, value)| value.map(|v| (name, v)))
    .collect();
    let names: Vec<&str> = columns.iter().map(|(n, _)| *n).collect();
    let marks: Vec<String> = (1..=columns.len()).map(|i| format!("?{i}")).collect();
    conn.execute(
        &format!(
            "INSERT INTO transcript_segments ({}) VALUES ({})",
            names.join(", "),
            marks.join(", ")
        ),
        rusqlite::params_from_iter(columns.into_iter().map(|(_, v)| v)),
    )?;
    let id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO transcript_fts (rowid, text) VALUES (?1, ?2)",
        (id, turn.text),
    )?;
    Ok(id)
}

/// Hide a turn that is not hidden yet. Returns whether it was.
///
/// # Errors
/// If the database refuses.
pub fn hide(conn: &Connection, id: i64, reason: &HiddenReason) -> rusqlite::Result<bool> {
    let changed = conn.execute(
        "UPDATE transcript_segments SET hidden_reason = ?1
         WHERE id = ?2 AND hidden_reason IS NULL",
        (reason.to_string(), id),
    )?;
    Ok(changed == 1)
}

/// Show a turn again that was hidden for `reason`. Returns whether it was.
///
/// # Errors
/// If the database refuses.
pub fn unhide(conn: &Connection, id: i64, reason: &HiddenReason) -> rusqlite::Result<bool> {
    let changed = conn.execute(
        "UPDATE transcript_segments SET hidden_reason = NULL
         WHERE id = ?1 AND hidden_reason = ?2 AND superseded_by IS NULL",
        (id, reason.to_string()),
    )?;
    Ok(changed == 1)
}

/// Hide a turn only if it is current: neither hidden nor superseded. One
/// statement, so of two concurrent claims only one wins.
///
/// # Errors
/// If the database refuses.
pub fn claim(conn: &Connection, id: i64, reason: &HiddenReason) -> rusqlite::Result<bool> {
    let changed = conn.execute(
        "UPDATE transcript_segments SET hidden_reason = ?1
         WHERE id = ?2 AND hidden_reason IS NULL AND superseded_by IS NULL",
        (reason.to_string(), id),
    )?;
    Ok(changed == 1)
}

/// Hide the current live turns starting in `[from, to)`: the archive pass has
/// written that span. By time, because a live turn has no audio segment.
///
/// # Errors
/// If the database refuses.
pub fn reconcile_live(conn: &Connection, from: &str, to: &str) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE transcript_segments SET hidden_reason = ?1
         WHERE asr_model = ?2 AND superseded_by IS NULL AND hidden_reason IS NULL
           AND start_utc >= ?3 AND start_utc < ?4",
        rusqlite::params![
            HiddenReason::LiveReconciled.to_string(),
            LIVE_MODEL,
            from,
            to
        ],
    )
}

/// Point `old` at the turn that replaces it.
///
/// # Errors
/// If the database refuses.
pub fn supersede(conn: &Connection, old: i64, new: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE transcript_segments SET superseded_by = ?1 WHERE id = ?2",
        (new, old),
    )?;
    Ok(())
}

/// Record which voice a pass heard in a turn, keeping its boundaries.
///
/// # Errors
/// If the database refuses.
pub fn set_cluster(conn: &Connection, id: i64, cluster: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE transcript_segments SET speaker_cluster = ?1 WHERE id = ?2",
        (cluster, id),
    )?;
    Ok(())
}

/// Set or clear the name a person gave one turn.
///
/// # Errors
/// If the database refuses.
pub fn set_label(conn: &Connection, id: i64, name: Option<&str>) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE transcript_segments SET speaker_label = ?1 WHERE id = ?2",
        (name, id),
    )?;
    Ok(())
}

/// Rename the current turn a correction produced.
///
/// # Errors
/// If the database refuses.
pub fn label_correction(conn: &Connection, original: i64, speaker: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE transcript_segments SET speaker_label = ?1
         WHERE provenance = ?2 AND asr_model = ?3 AND superseded_by IS NULL",
        (
            speaker,
            Provenance::Correction(original).to_string(),
            HUMAN_MODEL,
        ),
    )?;
    Ok(())
}

/// Name, or clear, one voice on every current turn of a session.
///
/// # Errors
/// If the database refuses.
pub fn label_voice(
    conn: &Connection,
    source: &str,
    cluster: &str,
    name: Option<&str>,
) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE transcript_segments SET speaker_label = ?1 WHERE id IN (
             SELECT ts.id FROM transcript_segments ts
             JOIN audio_segments a ON a.id = ts.audio_segment_id
             WHERE a.source_id = ?2 AND ts.speaker_cluster = ?3
               AND ts.superseded_by IS NULL)",
        (name, source, cluster),
    )
}

/// Store a voiceprint's guess for a turn.
///
/// # Errors
/// If the database refuses.
pub fn set_guess(conn: &Connection, id: i64, person: &str, score: f64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE transcript_segments SET speaker_guess = ?1, speaker_score = ?2 WHERE id = ?3",
        rusqlite::params![person, score, id],
    )?;
    Ok(())
}

/// Store a re-match's guess and when it was made.
///
/// # Errors
/// If the database refuses.
pub fn set_match(
    conn: &Connection,
    id: i64,
    person: &str,
    score: f64,
    now: &Stamp,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE transcript_segments
            SET speaker_guess = ?1, speaker_score = ?2, speaker_matched_utc = ?3
          WHERE id = ?4",
        rusqlite::params![person, score, now, id],
    )?;
    Ok(())
}

/// Record that a turn was matched at `now` with no change of guess.
///
/// # Errors
/// If the database refuses.
pub fn stamp_matched(conn: &Connection, id: i64, now: &Stamp) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE transcript_segments SET speaker_matched_utc = ?1 WHERE id = ?2",
        rusqlite::params![now, id],
    )?;
    Ok(())
}

/// Delete every turn of one audio segment: a deleted upload.
///
/// # Errors
/// If the database refuses.
pub fn delete_for_audio(conn: &Connection, audio_segment_id: i64) -> rusqlite::Result<usize> {
    conn.execute(
        "DELETE FROM transcript_segments WHERE audio_segment_id = ?1",
        [audio_segment_id],
    )
}
