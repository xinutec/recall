//! The household pause's messages, one type each for recalld, the recorders'
//! mirror and the web app: copies were held in step by hand. The bytes are
//! pinned by `tests/fixtures/wire/`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The wire shape of `GET /api/capture`. Field order and spelling are a
/// contract: `stateToken` is a hash over this object, so a renamed or reordered
/// field changes every client's long-poll. The bools are the wire shape, so an
/// enum would change the contract rather than tidy it.
#[expect(
    clippy::struct_excessive_bools,
    reason = "the bools are the wire shape (doc above)"
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct CaptureState {
    pub running: bool,
    #[serde(rename = "pausedUntil")]
    pub paused_until: Option<String>,
    #[serde(rename = "desiredRunning")]
    pub desired_running: bool,
    #[serde(rename = "desiredPausedUntil")]
    pub desired_paused_until: Option<String>,
    pub settled: bool,
    #[serde(rename = "micReachable")]
    pub mic_reachable: bool,
    #[serde(rename = "stateToken")]
    pub state_token: String,
}

/// `POST /sync/capture`'s body: what the Mac reports it has applied, each
/// mirror pass.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Applied {
    pub running: bool,
    /// The ISO resume-by it applied; absent while recording.
    #[serde(default)]
    pub paused_until: Option<String>,
    /// Each source's last proved recording time.
    #[serde(default)]
    pub source_liveness: BTreeMap<String, String>,
    /// Seconds to hang while the intent still equals `known_intent`.
    #[serde(default)]
    pub wait: f64,
    /// The intent the Mac has applied; `None` means running.
    #[serde(default)]
    pub known_intent: Option<String>,
}

/// `POST /sync/capture`'s answer: the intent, for the Mac to mirror onto its
/// pause file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Intent {
    pub paused_until: Option<String>,
}
