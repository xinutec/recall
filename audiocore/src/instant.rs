//! One spelling of an ISO-8601 instant, and one reader of them.
//!
//! Stored timestamps are compared and ordered as text (`WHERE start_utc <
//! ?1`), so two spellings of one moment are two different values. The one
//! spelling is Python's `datetime.isoformat()` of a UTC instant: `+00:00`
//! rather than `Z`, no fraction when it is zero and six digits when it is not.
//! Any other offset is converted. The meaning plane refuses a write in any other
//! spelling (migration v47).

use chrono::{DateTime, FixedOffset, NaiveDateTime, SecondsFormat, TimeZone, Utc};

fn format_for(subsec_micros: u32) -> SecondsFormat {
    if subsec_micros == 0 {
        SecondsFormat::Secs
    } else {
        SecondsFormat::Micros
    }
}

/// Spell a UTC instant in the stored form.
#[must_use]
pub fn python_isoformat_utc(when: DateTime<Utc>) -> String {
    when.to_rfc3339_opts(format_for(when.timestamp_subsec_micros()), false)
}

/// Re-spell an instant in the stored form, converted to UTC, or `None` if it
/// does not parse.
#[must_use]
pub fn respell_utc(value: &str) -> Option<String> {
    parse(value).map(|t| python_isoformat_utc(t.with_timezone(&Utc)))
}

/// Parse an instant as `datetime.fromisoformat` does: `Z` or an offset, or no
/// offset at all, which is taken as UTC rather than refused.
#[must_use]
pub fn parse(value: &str) -> Option<DateTime<FixedOffset>> {
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Some(parsed);
    }
    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(value, format) {
            return Some(Utc.from_utc_datetime(&naive).fixed_offset());
        }
    }
    None
}

/// [`parse`], as UTC, for a reader that compares instants rather than text.
#[must_use]
pub fn parse_utc(value: &str) -> Option<DateTime<Utc>> {
    parse(value).map(|t| t.with_timezone(&Utc))
}

/// A serde reader for a stored instant.
pub fn de<'de, D>(deserializer: D) -> Result<DateTime<Utc>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize as _;
    let raw = String::deserialize(deserializer)?;
    parse_utc(&raw).ok_or_else(|| serde::de::Error::custom(format!("not an instant: {raw}")))
}

/// [`de`] for an optional field.
pub fn de_opt<'de, D>(deserializer: D) -> Result<Option<DateTime<Utc>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize as _;
    match Option::<String>::deserialize(deserializer)? {
        None => Ok(None),
        Some(text) => parse_utc(&text)
            .map(Some)
            .ok_or_else(|| serde::de::Error::custom(format!("not an instant: {text}"))),
    }
}
