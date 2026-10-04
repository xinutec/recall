use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};

/// A moment, in whole microseconds since the Unix epoch, UTC.
///
/// Microseconds because stored instants carry them (word timings, live lines);
/// an integer because two text spellings of one moment compared unequal. Text
/// exists only at the edges: [`Instant::parse`] reads every spelling stored
/// today, and [`Instant::to_utc`] hands chrono to whoever formats.
// No derived `Deserialize`: it would build an `Instant` without the range check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
pub struct Instant(i64);

impl Instant {
    /// `None` outside chrono's range (about 262,000 years either side of 1970),
    /// so every `Instant` converts to a `DateTime` without a fallible step.
    pub fn from_micros(micros: i64) -> Option<Self> {
        DateTime::from_timestamp_micros(micros).map(|_| Self(micros))
    }

    pub const fn micros(self) -> i64 {
        self.0
    }

    pub fn from_utc(when: DateTime<Utc>) -> Self {
        Self(when.timestamp_micros())
    }

    pub fn to_utc(self) -> DateTime<Utc> {
        // Every constructor checked the range.
        DateTime::from_timestamp_micros(self.0).unwrap_or_default()
    }

    /// Every spelling the archive holds: RFC 3339 with `Z` or an offset, or no
    /// offset at all, which is UTC. Precision beyond microseconds is refused
    /// rather than rounded, so no stored value maps to a moment it is not.
    pub fn parse(text: &str) -> Option<Self> {
        let when = DateTime::parse_from_rfc3339(text)
            .map(|t| t.with_timezone(&Utc))
            .ok()
            .or_else(|| {
                ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"]
                    .iter()
                    .find_map(|f| NaiveDateTime::parse_from_str(text, f).ok())
                    .map(|naive| Utc.from_utc_datetime(&naive))
            })?;
        (when.timestamp_subsec_nanos() % 1_000 == 0).then(|| Self::from_utc(when))
    }

    /// `seconds` later (earlier when negative), to the microsecond; `None` past
    /// the representable range.
    pub fn plus_seconds(self, seconds: f64) -> Option<Self> {
        let delta = (seconds * 1e6).round();
        if !delta.is_finite() || delta.abs() >= 9.0e18 {
            return None;
        }
        #[expect(
            clippy::cast_possible_truncation,
            reason = "finite and inside i64, checked above"
        )]
        let delta = delta as i64;
        Self::from_micros(self.0.checked_add(delta)?)
    }
}
