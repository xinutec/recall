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

/// A serde writer for an instant, in the stored spelling; [`de`] reads it.
///
/// # Errors
/// As the serializer's.
pub fn ser<S>(when: &DateTime<Utc>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&python_isoformat_utc(*when))
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

/// An instant in the stored spelling.
///
/// Built only from a real instant, so a writer taking one cannot be handed a
/// placeholder or another spelling; a string parameter accepted `"now"`.
/// Read from any spelling [`parse`] takes, re-spelled; refused otherwise.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Deserialize)]
#[serde(try_from = "String")]
pub struct Stamp(String);

impl TryFrom<String> for Stamp {
    type Error = String;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::parse(&raw).ok_or_else(|| format!("not an instant: {raw:?}"))
    }
}

impl Stamp {
    pub fn of(when: DateTime<Utc>) -> Self {
        Self(python_isoformat_utc(when))
    }

    pub fn now() -> Self {
        Self::of(Utc::now())
    }

    /// Any spelling [`parse`] reads, converted; `None` if it is not an instant.
    pub fn parse(raw: &str) -> Option<Self> {
        respell_utc(raw).map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The instant it spells.
    #[must_use]
    pub fn utc(&self) -> DateTime<Utc> {
        parse_utc(&self.0).expect("a Stamp is built only from an instant")
    }
}

impl From<DateTime<Utc>> for Stamp {
    fn from(when: DateTime<Utc>) -> Self {
        Self::of(when)
    }
}

impl From<transcript::Instant> for Stamp {
    fn from(at: transcript::Instant) -> Self {
        Self::of(at.to_utc())
    }
}

impl std::fmt::Display for Stamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl rusqlite::ToSql for Stamp {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(self.0.as_str().into())
    }
}

impl rusqlite::types::FromSql for Stamp {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        let raw = value.as_str()?;
        Self::parse(raw).ok_or_else(|| {
            rusqlite::types::FromSqlError::Other(format!("not an instant: {raw:?}").into())
        })
    }
}

/// An instant in the ingest plane's spelling: UTC, whole seconds, `Z`
/// (`2026-09-05T12:00:00Z`). The meaning plane spells instants otherwise
/// ([`Stamp`]); both are compared as text, so the types keep a bound of one
/// from meeting a column of the other. A fraction of a second is dropped, as
/// a segment's name drops it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
pub struct IngestStamp(String);

impl IngestStamp {
    #[must_use]
    pub fn of(when: DateTime<Utc>) -> Self {
        Self(when.format("%Y-%m-%dT%H:%M:%SZ").to_string())
    }

    #[must_use]
    pub fn now() -> Self {
        Self::of(Utc::now())
    }

    /// Any spelling [`parse`] reads, converted; `None` if it is not an instant.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        parse_utc(raw).map(Self::of)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The instant it spells.
    #[must_use]
    pub fn utc(&self) -> DateTime<Utc> {
        parse_utc(&self.0).expect("an IngestStamp is built only from an instant")
    }
}

impl std::fmt::Display for IngestStamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl rusqlite::ToSql for IngestStamp {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(self.0.as_str().into())
    }
}

impl rusqlite::types::FromSql for IngestStamp {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        let raw = value.as_str()?;
        Self::parse(raw).ok_or_else(|| {
            rusqlite::types::FromSqlError::Other(format!("not an instant: {raw:?}").into())
        })
    }
}
