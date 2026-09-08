//! Reading the ISO-8601 instants Python wrote.
//!
//! The doctor only ever READS this archive, so it needs the parse half of
//! `recalld::instant` and not the spelling half: `Z` and `+00:00` are the same
//! moment here, and nothing is compared as text. A naive timestamp is assumed
//! UTC rather than refused, which is what every writer on the Mac does.

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};

/// Parse an instant, or `None` if it does not look like one.
pub fn parse(value: &str) -> Option<DateTime<Utc>> {
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Some(parsed.with_timezone(&Utc));
    }
    // No offset at all. Python's `datetime.fromisoformat` accepts this and the
    // readers here treat it as UTC — a TypeError comparing naive to aware is
    // the failure this avoids.
    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(value, format) {
            return Some(Utc.from_utc_datetime(&naive));
        }
    }
    None
}

pub fn de<'de, D>(deserializer: D) -> Result<DateTime<Utc>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize as _;
    let raw = String::deserialize(deserializer)?;
    parse(&raw).ok_or_else(|| serde::de::Error::custom(format!("not an instant: {raw}")))
}

pub fn de_opt<'de, D>(deserializer: D) -> Result<Option<DateTime<Utc>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize as _;
    let raw = Option::<String>::deserialize(deserializer)?;
    match raw {
        None => Ok(None),
        Some(text) => parse(&text)
            .map(Some)
            .ok_or_else(|| serde::de::Error::custom(format!("not an instant: {text}"))),
    }
}
