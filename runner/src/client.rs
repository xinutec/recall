//! Talking to recalld: lease, fetch, ack.

use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;

/// One unit of work, exactly as the queue hands it over.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Job {
    pub id: i64,
    pub kind: audiocore::job::Kind,
    /// The blob to work on, under `source`.
    pub filename: String,
    /// Which recorder it came from, as recalld serves it. Never guessed here.
    pub source: String,
    /// For `enroll-speaker`: which stretches of the clip to embed, in seconds
    /// from its start. Absent for every other kind.
    #[serde(default)]
    pub spans: Vec<Span>,
}

/// One stretch of a clip to embed, as recalld serves it. It carries no name:
/// the fleet reads the label when it writes the print, so a turn re-assigned
/// while the model runs is filed under its current name.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Span {
    pub segment_id: i64,
    pub start_s: f64,
    pub end_s: f64,
}

#[derive(Deserialize)]
struct LeaseBody {
    job: Option<Job>,
}

#[derive(Deserialize)]
struct PromptBody {
    prompt: Option<String>,
}

#[derive(Debug)]
pub enum Error {
    Http(String),
    Body(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(e) => write!(f, "recalld: {e}"),
            Self::Body(e) => write!(f, "unreadable response: {e}"),
        }
    }
}

impl std::error::Error for Error {}

/// A recalld the runner can reach. The token is the read plane's sync token,
/// not a device token: the runner reads blobs and the queue, which no recorder
/// may do.
pub struct Client {
    base: String,
    token: String,
    agent: ureq::Agent,
}

impl Client {
    #[must_use]
    pub fn new(base: &str, token: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_owned(),
            token: token.to_owned(),
            agent: ureq::AgentBuilder::new().build(),
        }
    }

    fn auth(&self, req: ureq::Request) -> ureq::Request {
        req.set("authorization", &format!("Bearer {}", self.token))
    }

    /// Take the next job of a kind this runner can do, or `None` when there is
    /// none.
    ///
    /// `kinds` is always sent, although recalld treats an absent list as
    /// `transcribe-segment` alone; relying on that default would tie any other
    /// runner's correctness to recalld's.
    ///
    /// # Errors
    /// If recalld is unreachable or answers something unreadable.
    pub fn lease(&self, kinds: &[audiocore::job::Kind]) -> Result<Option<Job>, Error> {
        let kinds: Vec<&str> = kinds.iter().map(|k| k.as_str()).collect();
        let url = format!("{}/work/v1/lease?kinds={}", self.base, kinds.join(","));
        let response = self
            .auth(self.agent.put(&url))
            .call()
            .map_err(|e| Error::Http(e.to_string()))?;
        let body: LeaseBody = serde_json::from_reader(response.into_reader())
            .map_err(|e| Error::Body(e.to_string()))?;
        Ok(body.job)
    }

    /// Fetch a job's audio into `dst`.
    ///
    /// # Errors
    /// If the blob cannot be fetched or written.
    pub fn fetch_blob(&self, source: &str, filename: &str, dst: &Path) -> Result<(), Error> {
        let url = format!("{}/ingest/v1/blob/{source}/{filename}", self.base);
        let response = self
            .auth(self.agent.get(&url))
            .call()
            .map_err(|e| Error::Http(e.to_string()))?;
        let mut bytes = Vec::new();
        response
            .into_reader()
            .read_to_end(&mut bytes)
            .map_err(|e| Error::Body(e.to_string()))?;
        std::fs::write(dst, &bytes).map_err(|e| Error::Body(e.to_string()))?;
        Ok(())
    }

    /// Retire a job with its result.
    ///
    /// # Errors
    /// If recalld refuses or is unreachable.
    pub fn finish(&self, id: i64, result: &str) -> Result<(), Error> {
        self.auth(
            self.agent
                .put(&format!("{}/work/v1/jobs/{id}/done", self.base)),
        )
        .send_string(result)
        .map_err(|e| Error::Http(e.to_string()))?;
        Ok(())
    }

    /// Push provisional live turns to the instant feed.
    ///
    /// Lossy on purpose (see [`crate::live`]): the caller logs a failure and
    /// carries on, because the archive pass supersedes these turns.
    ///
    /// # Errors
    /// If recalld refuses or is unreachable.
    pub fn push_live(&self, turns: &[LiveTurn]) -> Result<usize, Error> {
        let response = self
            .auth(self.agent.post(&format!("{}/sync/live", self.base)))
            .send_json(serde_json::json!({ "turns": turns }))
            .map_err(|e| Error::Http(e.to_string()))?;
        let body: LiveStoredBody = serde_json::from_reader(response.into_reader())
            .map_err(|e| Error::Body(e.to_string()))?;
        Ok(body.stored)
    }

    /// The vocabulary, as Whisper's `initial_prompt`.
    ///
    /// Fetched by the caller and sent with each job: a shim holds no database,
    /// and storing it on the job at queue time would keep newly learned names
    /// from reaching jobs already queued.
    ///
    /// `Ok(None)` means the vocabulary is empty: no biasing. The runner treats
    /// an error as fatal, because an unbiased transcript has to be redone; the
    /// live tier does not, because a live turn is superseded within the hour.
    ///
    /// # Errors
    /// If recalld is unreachable or answers something unreadable.
    pub fn prompt(&self) -> Result<Option<String>, Error> {
        let url = format!("{}/sync/vocabulary/prompt", self.base);
        let response = self
            .auth(self.agent.get(&url))
            .call()
            .map_err(|e| Error::Http(e.to_string()))?;
        let body: PromptBody = serde_json::from_reader(response.into_reader())
            .map_err(|e| Error::Body(e.to_string()))?;
        Ok(body.prompt.filter(|p| !p.trim().is_empty()))
    }
}

/// One provisional turn, exactly as `POST /sync/live` takes it
/// (`recalld::work::LiveTurn`).
///
/// ⚠ `snake_case` on the wire, deliberately: the server matches `asr_model`.
/// A `rename_all = "camelCase"`, as the browsing plane's types carry, would
/// make every push a 422 that the agent only logs.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LiveTurn {
    pub start: String,
    pub end: String,
    pub text: String,
    pub asr_model: String,
    pub language: Option<String>,
}

#[derive(Deserialize)]
struct LiveStoredBody {
    stored: usize,
}
