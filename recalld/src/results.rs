//! A clip's current model output (#1911), read by its [`ClipId`].
//!
//! Model output is re-derivable from the audio, so only the current result is
//! kept: one per clip and kind, in the job that produced it. A retranscription
//! replaces it. This is the one place that reaches a job from a clip, and each
//! kind has its own reader, so a result can only be read as what it is.

use audiocore::job::Kind;
use audiocore::shim::{Stored, asr, voices};
use rusqlite::{Connection, OptionalExtension};
use serde::de::DeserializeOwned;
use transcript::{ClipId, Instant};

crate::statements! {
    CURRENT: Ingest =
        "SELECT j.result, j.done_utc FROM jobs j
         JOIN clips c ON c.filename = j.filename
         WHERE c.id = ?1 AND j.kind = ?2 AND j.result IS NOT NULL AND j.done_utc IS NOT NULL";
}

/// What the model made of a clip.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome<T> {
    /// The model's answer, finished at `done`.
    Answer { done: Instant, answer: T },
    /// The model refused the clip, and why. Distinct from "not yet done",
    /// which is `None` around this.
    Refused { done: Instant, why: String },
}

/// A stored result that is not what its kind promises.
#[derive(Debug)]
pub enum ResultError {
    Db(rusqlite::Error),
    /// The finish time is not an instant.
    Done(String),
    /// The body does not parse as its kind's reply.
    Body(serde_json::Error),
}

impl std::fmt::Display for ResultError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db(err) => write!(f, "{err}"),
            Self::Done(raw) => write!(f, "stored finish time {raw:?} is not an instant"),
            Self::Body(err) => write!(f, "stored result does not parse: {err}"),
        }
    }
}

impl std::error::Error for ResultError {}

impl From<rusqlite::Error> for ResultError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Db(err)
    }
}

/// What the model heard in the clip; `None` before it has been transcribed.
pub fn transcription(
    ingest: &Connection,
    clip: ClipId,
) -> Result<Option<Outcome<asr::Reply>>, ResultError> {
    current(ingest, clip, Kind::TranscribeSegment)
}

/// Who spoke when in the clip; `None` before it has been diarized.
pub fn diarization(
    ingest: &Connection,
    clip: ClipId,
) -> Result<Option<Outcome<voices::Diarization>>, ResultError> {
    current(ingest, clip, Kind::DiarizeSegment)
}

fn current<T: DeserializeOwned>(
    ingest: &Connection,
    clip: ClipId,
    kind: Kind,
) -> Result<Option<Outcome<T>>, ResultError> {
    let Some((body, done)) = CURRENT
        .query_row(ingest, (clip.stored(), kind), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .optional()?
    else {
        return Ok(None);
    };
    let done = Instant::parse(&done).ok_or(ResultError::Done(done))?;
    let stored = Stored::<T>::parse(&body).map_err(ResultError::Body)?;
    Ok(Some(match (stored.ok, stored.result) {
        (true, Some(answer)) => Outcome::Answer { done, answer },
        (_, _) => Outcome::Refused {
            done,
            why: stored
                .error
                .unwrap_or_else(|| "no answer and no reason".to_owned()),
        },
    }))
}
