//! The queue's job kinds, shared by recalld (which derives and leases them) and
//! the runner (which asks for and does them). One enum, so the two cannot spell
//! a kind differently: a misspelt kind leased nothing and failed nowhere.

use std::fmt;
use std::str::FromStr;

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// One microphone's clip, or an uploaded meeting, transcribed.
    TranscribeSegment,
    /// Who spoke when over one microphone's clip.
    DiarizeSegment,
    /// A person-named turn made into a reference voiceprint.
    EnrollSpeaker,
}

impl Kind {
    pub const ALL: [Self; 3] = [
        Self::TranscribeSegment,
        Self::DiarizeSegment,
        Self::EnrollSpeaker,
    ];

    /// The stored and wire spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TranscribeSegment => "transcribe-segment",
            Self::DiarizeSegment => "diarize-segment",
            Self::EnrollSpeaker => "enroll-speaker",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A spelling no kind has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownKind(pub String);

impl fmt::Display for UnknownKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown job kind {:?}", self.0)
    }
}

impl std::error::Error for UnknownKind {}

impl FromStr for Kind {
    type Err = UnknownKind;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|k| k.as_str() == raw)
            .ok_or_else(|| UnknownKind(raw.to_owned()))
    }
}

impl ToSql for Kind {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.as_str()))
    }
}

impl FromSql for Kind {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        value
            .as_str()?
            .parse()
            .map_err(|err| FromSqlError::Other(Box::new(err)))
    }
}
