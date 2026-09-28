//! What `GET /sync/record/health` answers: the record's own faults, measured
//! by recalld and graded by the doctor.

use serde::{Deserialize, Serialize};

/// Since the window start: requests that failed on the server's side, and
/// minutes a microphone shows twice.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordHealth {
    pub faults: Faults,
    pub doubled: Doubled,
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
