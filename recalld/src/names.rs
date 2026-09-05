//! The segment-name grammar every recorder speaks:
//! `<source>-YYYYMMDDTHHMMSS.<ext>`, UTC, stamped by the recorder's own clock
//! at segment open (docs/architecture.md, decision 4). The name is the only
//! timing metadata a segment carries, so parsing it strictly here is what
//! keeps the whole downstream — alignment, the room builder, the timeline —
//! reading honest clocks. A name that does not parse is refused at the door,
//! never guessed at.

use chrono::NaiveDateTime;

/// The closed set of containers a recorder may deliver. FLAC is the target
/// (decision 1); the rest are what existing capture paths produce today,
/// accepted because the protocol is container-agnostic and a recorder flips
/// formats independently. An enum so the set is parsed once, here, and every
/// consumer downstream matches a type rather than a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Extension {
    Flac,
    Opus,
    Ogg,
    Wav,
}

impl Extension {
    pub fn parse(ext: &str) -> Option<Self> {
        match ext {
            "flac" => Some(Self::Flac),
            "opus" => Some(Self::Opus),
            "ogg" => Some(Self::Ogg),
            "wav" => Some(Self::Wav),
            _ => None,
        }
    }

    /// The MIME type a blob answers with. `.opus` is an Ogg container.
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Flac => "audio/flac",
            Self::Wav => "audio/wav",
            Self::Opus | Self::Ogg => "audio/ogg",
        }
    }
}

/// A validated segment name, decomposed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentName {
    pub source: String,
    /// ISO-8601 with a trailing `Z`, e.g. `2026-09-05T12:00:00Z` — the form
    /// the sqlite rows store and sort by.
    pub start_utc: String,
    pub ext: Extension,
}

/// Why a name was refused — carried into the 400 body so a recorder's log
/// says what to fix rather than "bad request".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameError {
    BadSource,
    WrongPrefix,
    BadStamp,
    BadExtension,
}

impl NameError {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BadSource => "source id must be [a-z0-9][a-z0-9_-]*, at most 64 chars",
            Self::WrongPrefix => "filename must be <source>-<stamp>.<ext>",
            Self::BadStamp => "stamp must be a valid YYYYMMDDTHHMMSS UTC instant",
            Self::BadExtension => "extension must be one of flac/opus/ogg/wav",
        }
    }
}

/// A filesystem-safe source id: one source = one storage directory, so the id
/// is a path component and the grammar excludes everything a path could
/// interpret ('.', '/', case games). Matches the ids in use: usb, geb,
/// pixel5, iphone11, room.
pub fn valid_source(source: &str) -> bool {
    let mut chars = source.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    source.len() <= 64
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// Parse `filename` as a segment of `source`, or say exactly why not.
pub fn parse(source: &str, filename: &str) -> Result<SegmentName, NameError> {
    if !valid_source(source) {
        return Err(NameError::BadSource);
    }
    let rest = filename
        .strip_prefix(source)
        .and_then(|r| r.strip_prefix('-'))
        .ok_or(NameError::WrongPrefix)?;
    let (stamp, ext) = rest.split_once('.').ok_or(NameError::WrongPrefix)?;
    let ext = Extension::parse(ext).ok_or(NameError::BadExtension)?;
    if stamp.len() != 15 {
        return Err(NameError::BadStamp);
    }
    let parsed =
        NaiveDateTime::parse_from_str(stamp, "%Y%m%dT%H%M%S").map_err(|_| NameError::BadStamp)?;
    Ok(SegmentName {
        source: source.to_owned(),
        start_utc: parsed.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        ext,
    })
}
