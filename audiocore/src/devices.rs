//! A mic app's heartbeat, one type for recalld and the LAN relay. The bytes
//! are pinned by `tests/fixtures/wire/`.

use serde::{Deserialize, Serialize};

/// `POST /api/devices/heartbeat`. Every field but `device` is optional: an
/// older app still counts as alive. No `at`: the fleet stamps it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Heartbeat {
    pub device: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streaming: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charging: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mic_ok: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped_bytes: Option<i64>,
    /// Stamped by the relay, never copied from the phone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via_lan: Option<bool>,
}
