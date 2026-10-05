//! The segment-name grammar every recorder speaks:
//! `<source>-YYYYMMDDTHHMMSS[.phone].<ext>`, UTC, stamped by the recorder's own
//! clock at segment open (docs/architecture.md, decision 4). The name is the
//! only timing metadata a segment carries, so every reader parses it here.
//!
//! `.phone` marks a phone's own copy of a minute whose stream the host also
//! cuts. The two open within seconds of each other, often in the same one, and
//! would otherwise share a name.

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use std::path::{Path, PathBuf};

/// The containers a producer may deliver. FLAC is the target (decision 1); the
/// rest come from capture paths and uploads. Must cover
/// `recalld::upload::AUDIO_SUFFIXES`: an upload is fetched back through
/// `/ingest/v1/blob`, which parses the name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Extension {
    Flac,
    Opus,
    Ogg,
    Wav,
    Mp3,
    Mp4,
    Aac,
    Webm,
}

impl Extension {
    pub fn parse(ext: &str) -> Option<Self> {
        match ext {
            "flac" => Some(Self::Flac),
            "opus" => Some(Self::Opus),
            "ogg" => Some(Self::Ogg),
            "wav" => Some(Self::Wav),
            "mp3" => Some(Self::Mp3),
            // `.m4a` is an MP4 container by another name; both are served as one.
            "m4a" | "mp4" => Some(Self::Mp4),
            "aac" => Some(Self::Aac),
            "webm" => Some(Self::Webm),
            _ => None,
        }
    }

    /// Whether a recorder's segment ring writes this container; the rest arrive
    /// by upload.
    pub fn recorded(self) -> bool {
        matches!(self, Self::Flac | Self::Opus | Self::Ogg | Self::Wav)
    }

    /// The MIME type a blob answers with. `.opus` is an Ogg container.
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Flac => "audio/flac",
            Self::Wav => "audio/wav",
            Self::Opus | Self::Ogg => "audio/ogg",
            Self::Mp3 => "audio/mpeg",
            Self::Mp4 => "audio/mp4",
            Self::Aac => "audio/aac",
            Self::Webm => "audio/webm",
        }
    }
}

/// A validated segment name, decomposed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentName {
    pub source: String,
    /// ISO-8601 with a trailing `Z`, e.g. `2026-09-05T12:00:00Z`.
    pub start_utc: String,
    pub ext: Extension,
}

/// Why a name was refused, in the 400 body so a recorder's log says what to
/// fix.
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
            Self::WrongPrefix => "filename must be <source>-<stamp>[.phone].<ext>",
            Self::BadStamp => "stamp must be a valid YYYYMMDDTHHMMSS UTC instant",
            Self::BadExtension => "extension must be one of flac/opus/ogg/wav/mp3/m4a/mp4/aac/webm",
        }
    }
}

/// A filesystem-safe source id: [`transcript::SourceId`] holds the one rule.
pub fn valid_source(source: &str) -> bool {
    transcript::SourceId::parse(source).is_some()
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
    let ext = ext.strip_prefix("phone.").unwrap_or(ext);
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

/// The UTC start in a filename (the first `YYYYMMDDTHHMMSS`). Looser than
/// [`parse`]: the sweeps read files outside the strict grammar.
pub fn parse_segment_start(filename: &str) -> Option<DateTime<Utc>> {
    for start in 0..filename.len().saturating_sub(14) {
        // `.get`: a multibyte filename must not panic on a boundary.
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

/// The source's segment files (open, closed or stub), by name: chronological.
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
