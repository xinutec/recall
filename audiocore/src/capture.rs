//! Capture control's messages, one type each for recalld, the recorders'
//! mirror and the web app: copies were held in step by hand. The bytes are
//! pinned by `tests/fixtures/wire/`.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

/// What a person asked capture to do: nothing (off), record until a time, or
/// hold a window open without recording. Off unless someone opened a window,
/// so a forgotten phone or a press nobody made records nothing.
///
/// Spelled `off`, `record <until>` or `hold <until>` wherever it is kept: the
/// fleet's intent, the Mac's gate file, a phone's memory. Text that is none of
/// these is no window at all, a fault, and a fault records: losing a call to a
/// broken file is worse than recording one too many.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    Off,
    Record(DateTime<Utc>),
    Hold(DateTime<Utc>),
}

/// The gate file on a recorder's data root, holding its [`Window`] spelled:
/// a cross-process contract every recording agent checks.
pub const GATE_FILE: &str = "capture_window";

/// The window in `root`'s gate file; `None` when it is missing or does not
/// read, a fault, which records.
#[must_use]
pub fn read_gate(root: &std::path::Path) -> Option<Window> {
    Window::parse(&std::fs::read_to_string(root.join(GATE_FILE)).ok()?)
}

/// The longest window one press opens.
#[must_use]
pub fn max_window() -> Duration {
    Duration::hours(24)
}

impl Window {
    /// A window from `now` to `until`, at most [`max_window`] long; off when
    /// `until` is not after `now`.
    #[must_use]
    pub fn open(now: DateTime<Utc>, until: DateTime<Utc>) -> Self {
        Self::Record(until.min(now + max_window())).at(now)
    }

    /// `None` for text that spells no window.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if text == "off" {
            return Some(Self::Off);
        }
        let (verb, until) = text.split_once(' ')?;
        let until = crate::instant::parse_utc(until)?;
        match verb {
            "record" => Some(Self::Record(until)),
            "hold" => Some(Self::Hold(until)),
            _ => None,
        }
    }

    #[must_use]
    pub fn spell(self) -> String {
        let iso = |t: DateTime<Utc>| t.to_rfc3339_opts(SecondsFormat::Secs, true);
        match self {
            Self::Off => "off".to_owned(),
            Self::Record(until) => format!("record {}", iso(until)),
            Self::Hold(until) => format!("hold {}", iso(until)),
        }
    }

    /// The window as it stands at `now`: past its end, off.
    #[must_use]
    pub fn at(self, now: DateTime<Utc>) -> Self {
        match self.until() {
            Some(until) if until <= now => Self::Off,
            _ => self,
        }
    }

    #[must_use]
    pub fn records(self, now: DateTime<Utc>) -> bool {
        matches!(self.at(now), Self::Record(_))
    }

    #[must_use]
    pub const fn until(self) -> Option<DateTime<Utc>> {
        match self {
            Self::Off => None,
            Self::Record(until) | Self::Hold(until) => Some(until),
        }
    }

    /// Stop recording, keeping the end.
    #[must_use]
    pub const fn paused(self) -> Self {
        match self {
            Self::Record(until) => Self::Hold(until),
            other => other,
        }
    }

    /// Record again, to the same end.
    #[must_use]
    pub const fn unpaused(self) -> Self {
        match self {
            Self::Hold(until) => Self::Record(until),
            other => other,
        }
    }
}

/// The wire shape of `GET /api/capture`. Field order and spelling are a
/// contract: `stateToken` is a hash over this object, so a renamed or reordered
/// field changes every client's long-poll. The bools are the wire shape, so an
/// enum would change the contract rather than tidy it.
///
/// `running` is whether it records now; `until` the open window's end, `held`
/// whether that window is paused. No `until` while running: a fault, which
/// records.
#[expect(
    clippy::struct_excessive_bools,
    reason = "the bools are the wire shape (doc above)"
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct CaptureState {
    pub running: bool,
    pub until: Option<String>,
    pub held: bool,
    #[serde(rename = "desiredRunning")]
    pub desired_running: bool,
    #[serde(rename = "desiredUntil")]
    pub desired_until: Option<String>,
    #[serde(rename = "desiredHeld")]
    pub desired_held: bool,
    pub settled: bool,
    #[serde(rename = "micReachable")]
    pub mic_reachable: bool,
    #[serde(rename = "stateToken")]
    pub state_token: String,
}

impl CaptureState {
    /// The window wanted, for a recorder that applies it: `None` for a fault
    /// (recording with no end, or an end that does not read), which records.
    #[must_use]
    pub fn desired_window(&self) -> Option<Window> {
        let Some(until) = self.desired_until.as_deref() else {
            return (!self.desired_running).then_some(Window::Off);
        };
        let until = crate::instant::parse_utc(until)?;
        Some(if self.desired_held {
            Window::Hold(until)
        } else {
            Window::Record(until)
        })
    }
}

/// `POST /sync/capture`'s body: what the Mac reports it has applied, each
/// mirror pass.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Applied {
    pub running: bool,
    /// The gate file's [`Window`], spelled; absent when it is missing or
    /// unreadable, which records.
    pub window: Option<String>,
    /// Each source's last proved recording time.
    #[serde(default)]
    pub source_liveness: BTreeMap<String, String>,
    /// Seconds to hang while the intent still equals `known_intent`.
    #[serde(default)]
    pub wait: f64,
    /// The intent the Mac has applied, spelled; `None` before the first.
    #[serde(default)]
    pub known_intent: Option<String>,
}

/// `POST /sync/capture`'s answer: the intent, a [`Window`] spelled, for the
/// Mac to mirror onto its gate file. Required, so an older fleet's answer is
/// refused rather than read as no window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Intent {
    pub window: String,
}
