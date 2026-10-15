//! What `GET /sync/record/health` answers: the record's own faults, measured
//! by recalld and graded by the doctor.

use serde::{Deserialize, Serialize};

/// Since the window start: requests that failed on the server's side,
/// minutes a microphone shows twice, and minutes an operator deleted. And how
/// each background pass is doing now.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordHealth {
    pub faults: Faults,
    pub doubled: Doubled,
    pub passes: Vec<Pass>,
    /// Recorded since the window start and deleted since (`recalld
    /// forget-span`), oldest first: deliberate, so not lost.
    pub forgotten: Vec<ForgottenMinute>,
}

/// One deleted minute of a microphone, by the instant its recording began.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForgottenMinute {
    pub source: transcript::SourceId,
    #[serde(
        serialize_with = "crate::instant::ser",
        deserialize_with = "crate::instant::de"
    )]
    pub start_utc: chrono::DateTime<chrono::Utc>,
}

/// One background pass of the server, as it last went. Instants are RFC 3339.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Pass {
    pub name: String,
    /// The pause between passes, in seconds.
    pub every_s: u64,
    /// When the server began running it.
    pub since_utc: String,
    /// Switched off in this server, and why.
    pub off: Option<String>,
    /// A pass under way, since when.
    pub running_since_utc: Option<String>,
    /// When the last pass ended, failed or not.
    pub last_end_utc: Option<String>,
    /// Since when every pass has failed, or left items it could not do; with
    /// the newest error. `None` once a pass goes clean.
    pub failing: Option<Failing>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Failing {
    pub since_utc: String,
    pub error: String,
}

/// Requests answered 500: a save that did not happen, a page that did not load.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub struct Faults {
    pub count: usize,
    pub last: Option<Fault>,
}

/// One failed request, as the fault log keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Fault {
    pub utc: String,
    /// The route, as the server names it in its log.
    pub what: String,
    pub error: String,
}

/// Minutes whose speech a microphone shows from two clips: the same words
/// twice on the timeline, and counted twice everywhere else.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub struct Doubled {
    pub count: usize,
    pub last: Option<DoubledMinute>,
}

/// The newest doubled minute.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoubledMinute {
    pub source: String,
    pub start_utc: String,
}
