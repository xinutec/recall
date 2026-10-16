//! The work plane's messages, one type each for recalld and the runner: two
//! copies were held in step by hand. The bytes are pinned by
//! `tests/fixtures/wire/`.

use serde::{Deserialize, Serialize};

use crate::job::Kind;

/// `PUT /work/v1/lease`'s answer: `{"job": null}` when nothing is queued.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Lease {
    pub job: Option<Job>,
}

/// One unit of work, exactly as the queue hands it over.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Job {
    pub id: i64,
    pub kind: Kind,
    /// Fetched via `/ingest/v1/blob/<source>/<filename>`.
    pub filename: String,
    /// Carried, not parsed from the filename: `meeting-20260907-0905` is a
    /// source id, so no split on a hyphen is safe.
    pub source: String,
    /// For [`Kind::EnrollSpeaker`] only: the stretches to embed, filled at
    /// lease time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spans: Vec<Span>,
    /// For [`Kind::TranscribeSegment`] only: the language its session is pinned
    /// to; absent, the model guesses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

impl Job {
    /// The filename as a single path component, to write under the scratch
    /// directory; `None` for anything that would land elsewhere.
    #[must_use]
    pub fn scratch_name(&self) -> Option<&str> {
        let name = std::path::Path::new(&self.filename);
        (name.file_name() == Some(name.as_os_str()) && self.filename != "..")
            .then_some(self.filename.as_str())
    }
}

/// One stretch to embed. No name on the wire: the naming is read at write
/// time, so one renamed meanwhile enrols under its new name.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Span {
    /// The naming act's id (`edits.id`), echoed back with its print. Named for
    /// the runner's protocol, which predates the edit log.
    pub segment_id: i64,
    /// Seconds from the start of the clip.
    pub start_s: f64,
    pub end_s: f64,
}

/// `POST /sync/live`'s body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LiveTurns {
    pub turns: Vec<LiveTurn>,
}

/// One provisional turn: `snake_case`, unlike the browsing plane.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LiveTurn {
    pub start: String,
    pub end: String,
    pub text: String,
    pub asr_model: String,
    #[serde(default)]
    pub language: Option<String>,
}

/// `POST /sync/live`'s answer: how many were newly stored.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct LiveStored {
    pub stored: usize,
}

/// `GET /sync/vocabulary/prompt`'s answer: `null` when the glossary is empty.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Prompt {
    pub prompt: Option<String>,
}
