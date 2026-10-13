//! The passes' ledger (`pass_ledger` in `ingest.sqlite`): the terminal decision
//! a pass made about a clip, so the clip is not decided again. Deleting a row
//! lets the pass decide it afresh.

use audiocore::instant::Stamp;
use audiocore::job::Kind;
use rusqlite::types::ToSqlOutput;

crate::statements! {
    RECORD: Ingest =
        "INSERT OR REPLACE INTO pass_ledger (kind, filename, outcome, detail, decided_utc)
         VALUES (?1, ?2, ?3, NULL, ?4)";
}

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
    /// Another copy of the same minute already holds it.
    CoveredBySibling,
    /// The filename is not a segment name.
    Unnameable,
    // Any pass.
    /// The file or the stored result cannot be read.
    Unreadable,
    // Enrolment.
    Enrolled,
    NothingEnrolled,
    Refused,
}

impl Outcome {
    /// The stored spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Registered => "registered",
            Self::AlreadyRegistered => "already-registered",
            Self::CoveredBySibling => "covered-by-sibling",
            Self::Unnameable => "unnameable",
            Self::Unreadable => "unreadable",
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

/// Record a pass's terminal decision on a clip.
pub fn record(
    conn: &rusqlite::Connection,
    kind: impl Into<PassKind>,
    filename: &str,
    outcome: Outcome,
    now: &Stamp,
) -> rusqlite::Result<()> {
    RECORD.execute(conn, rusqlite::params![kind.into(), filename, outcome, now])?;
    Ok(())
}
