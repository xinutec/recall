//! What recalld tells the doctor about the live tier, one type each for both:
//! two copies were held in step by hand. The bytes are pinned by
//! `tests/fixtures/wire/`.

use serde::{Deserialize, Serialize};

/// `GET /sync/live/health`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LiveHealth {
    /// Median seconds between a live turn's end and its arrival.
    pub lag_median_s: Option<f64>,
    /// How many turns that median is over; a handful is noise.
    pub lag_samples: usize,
    /// The newest live turn's start, however old.
    pub newest_turn_utc: Option<String>,
    /// Seconds of audio the device recorders delivered in the window.
    pub delivered_s: f64,
    /// Seconds of that audio the speech scanner has measured.
    pub scanned_s: f64,
    /// Seconds of speech found within the scanned part.
    pub speech_s: f64,
}

/// What one device source delivered and heard in a window (`GET
/// /sync/heard` answers a list). Only measured clips count: otherwise a
/// lagging scanner looks like a deaf microphone.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Heard {
    pub source: String,
    pub delivered_s: f64,
    pub speech_s: f64,
}

impl Heard {
    #[must_use]
    pub fn new(source: &str, delivered_s: f64, speech_s: f64) -> Self {
        Self {
            source: source.to_owned(),
            delivered_s,
            speech_s,
        }
    }

    /// Speech per minute of delivered audio; zero if nothing was delivered.
    #[must_use]
    pub fn per_minute(&self) -> f64 {
        if self.delivered_s <= 0.0 {
            return 0.0;
        }
        self.speech_s * 60.0 / self.delivered_s
    }
}
