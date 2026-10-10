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
    /// For `transcribe-segment`: the language its session is pinned to;
    /// absent, the model guesses.
    #[serde(default)]
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

/// One stretch of a clip to embed. No name: the fleet reads the label when it
/// writes the print.
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

/// A recalld the runner can reach, with the sync token: it reads blobs and the
/// queue, which no device token may.
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

    /// The next job of a kind this runner can do. `kinds` is always sent,
    /// rather than relying on recalld's default.
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

    /// Push provisional live turns. Lossy on purpose: the archive pass
    /// supersedes them.
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

    /// The vocabulary as Whisper's `initial_prompt`, fetched per job so new
    /// names reach queued jobs. `Ok(None)`: empty, no biasing.
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

/// One provisional turn as `POST /sync/live` takes it
/// (`recalld::work::LiveTurn`): `snake_case`, unlike the browsing plane.
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
