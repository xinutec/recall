//! The segment-name grammar every recorder speaks:
//! `<source>-YYYYMMDDTHHMMSS.<ext>`, UTC, stamped by the recorder's own clock
//! at segment open (docs/architecture.md, decision 4). The name is the only
//! timing metadata a segment carries, so ONE crate parses it — recalld's
//! ingest door, audiod's sweeps and rebase, and the room builder all read
//! these functions rather than keeping grammars that could drift.

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use std::path::{Path, PathBuf};

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

const TS_FORMAT: &str = "%Y%m%dT%H%M%S";

/// The UTC start time embedded in a segment filename (the first
/// `YYYYMMDDTHHMMSS` token), or `None` for a file that carries none. Looser
/// than [`parse`] on purpose: the sweeps and the rebase read files that may
/// predate the strict grammar (arrival-stamped, derived copies), and a stamp
/// anywhere in the name is still a stamp.
pub fn parse_segment_start(filename: &str) -> Option<DateTime<Utc>> {
    for start in 0..filename.len().saturating_sub(14) {
        // .get: a multibyte filename must not panic the sweep on a boundary
        let Some(window) = filename.get(start..start + 15) else {
            continue;
        };
        if window.as_bytes()[8] != b'T' {
            continue;
        }
        if let Ok(naive) = NaiveDateTime::parse_from_str(window, TS_FORMAT) {
            return Some(Utc.from_utc_datetime(&naive));
        }
    }
    None
}

/// The source's segment files (any state: open, closed, stub), sorted by name —
/// which is chronological, because the name embeds the UTC start time.
pub fn segment_glob(out_dir: &Path, source_id: &str) -> Vec<PathBuf> {
    let prefix = format!("{source_id}-");
    let mut files: Vec<PathBuf> = std::fs::read_dir(out_dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with(&prefix))
                })
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}
