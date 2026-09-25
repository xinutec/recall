//! The passes' ledger (`pass_ledger` in `ingest.sqlite`): each terminal decision
//! a pass made about a clip without writing a turn, so the clip is not decided
//! again. Written turns are their own record; deleting them re-enables a clip.
//!
//! The outcome is one word from [`Outcome`]; counts go in `detail` as JSON, so
//! the table groups by outcome without parsing sentences.

use audiocore::instant::Stamp;
use audiocore::job::Kind;
use rusqlite::types::ToSqlOutput;

/// Which pass decided a clip: a job kind's, or registration's, which has no job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassKind {
    Job(Kind),
    Register,
}

impl PassKind {
    /// The stored spelling in `pass_ledger.kind`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Job(kind) => kind.as_str(),
            Self::Register => "register-segment",
        }
    }
}

impl From<Kind> for PassKind {
    fn from(kind: Kind) -> Self {
        Self::Job(kind)
    }
}

impl rusqlite::ToSql for PassKind {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.as_str().into())
    }
}

/// What a pass decided. Read by people, never branched on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    // Registration.
    Registered,
    AlreadyRegistered,
    /// A sibling file (the `.wav` beside the `.opus`) already holds the minute.
    CoveredBySibling,
    // Any pass.
    /// The filename is not a segment name.
    Unnameable,
    /// The file or the stored result cannot be read.
    Unreadable,
    /// The clip was deleted; its audio is never coming.
    Deleted,
    // Transcription.
    /// The clip's audio row has no readable end.
    Unspanned,
    /// Decided, and every turn was swept or refused.
    NothingToWrite,
    // Diarization.
    /// Speaker-split turns replaced the clip's turns.
    Aligned,
    /// The clip's turns were named in place. Detail: `turns`, `speakers`.
    Attributed,
    /// No words or no speaker spans.
    NothingAligned,
    /// Every turn dropped. Detail: `loops`, `corrected`.
    AllTurnsFiltered,
    /// The transcription carries no usable words.
    AllSegmentsLooped,
    /// Far less text than what exists. Detail: `new_chars`, `existing_chars`.
    CoverageGuard,
    /// Fewer turns than exist and nothing to name. Detail: `produced`,
    /// `existing`, `speakers`.
    Undiscriminating,
    // Enrolment.
    Enrolled,
    NothingEnrolled,
    Refused,
}

impl Outcome {
    /// The stored spelling. Older rows carry the same words.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Registered => "registered",
            Self::AlreadyRegistered => "already-registered",
            Self::CoveredBySibling => "covered-by-sibling",
            Self::Unnameable => "unnameable",
            Self::Unreadable => "unreadable",
            Self::Deleted => "deleted",
            Self::Unspanned => "unspanned",
            Self::NothingToWrite => "nothing-to-write",
            Self::Aligned => "aligned",
            Self::Attributed => "attributed",
            Self::NothingAligned => "nothing-aligned",
            Self::AllTurnsFiltered => "all-turns-filtered",
            Self::AllSegmentsLooped => "all-segments-looped",
            Self::CoverageGuard => "coverage-guard",
            Self::Undiscriminating => "undiscriminating",
            Self::Enrolled => "enrolled",
            Self::NothingEnrolled => "nothing",
            Self::Refused => "refused",
        }
    }
}

impl rusqlite::ToSql for Outcome {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.as_str().into())
    }
}

/// Record a pass's terminal decision on a clip, with its counts if any.
///
/// A reversal must clear both the pass's turns and these rows, or the clips it
/// declined stay decided for ever.
pub fn record(
    conn: &rusqlite::Connection,
    kind: impl Into<PassKind>,
    filename: &str,
    outcome: Outcome,
    detail: Option<&serde_json::Value>,
    now: &Stamp,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO pass_ledger (kind, filename, outcome, detail, decided_utc)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            kind.into(),
            filename,
            outcome,
            detail.map(serde_json::Value::to_string),
            now
        ],
    )?;
    Ok(())
}
