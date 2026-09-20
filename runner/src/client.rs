//! Talking to recalld: lease, fetch, ack.

use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;

/// One unit of work, exactly as the queue hands it over.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Job {
    pub id: i64,
    pub kind: String,
    /// The blob to work on, under `source`.
    pub filename: String,
    /// Which recorder it came from. Served by recalld from the ingest plane —
    /// NEVER guessed here, and never `room` by default.
    pub source: String,
    /// For `enroll-speaker`: which stretches of the clip to embed, in seconds
    /// from its start. Absent for every other kind.
    #[serde(default)]
    pub spans: Vec<Span>,
}

/// One stretch of a clip to embed, as recalld serves it.
///
/// ⚠ **No name.** The runner does not learn whose voice it is, and does not need
/// to: the fleet reads the label when it writes the print, so a turn re-assigned
/// while the model was running is filed under the name it has now.
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

/// A recalld the runner can reach. The sync token is the READ plane's
/// credential — the same one the Mac already uses — never a device token: the
/// runner reads blobs and the queue, which no recorder may do.
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
    /// ⚠ `kinds` is sent EXPLICITLY even though recalld defaults to
    /// `transcribe-room` when it is absent. That default exists so a runner
    /// deployed before the parameter keeps working; relying on it here would
    /// make a `voices` runner's correctness depend on which side deployed first.
    ///
    /// # Errors
    /// If recalld is unreachable or answers something unreadable.
    pub fn lease(&self, kinds: &[&str]) -> Result<Option<Job>, Error> {
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
    /// ⚠ Lossy on purpose — see `live`'s module note. The caller logs a failure
    /// and carries on, because the archive push carries these turns again.
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

    /// The household glossary, as Whisper's `initial_prompt`.
    ///
    /// Lives on the API's SYNC plane rather than recalld's own, because the
    /// vocabulary is in the MEANING store and recalld owns the audio plane. It
    /// moves with everything else at stage F.
    ///
    /// ⚠ Read ONCE at startup by the runner and carried on every job. The shim
    /// must not fetch it — a model process holds no database — and
    /// writing it onto each job at derivation time would pin it, so a name
    /// learned today would never reach a job queued yesterday.
    ///
    /// `Ok(None)` means the vocabulary is EMPTY, which is fine and means "no
    /// biasing". Failing to reach it is an error. What that costs depends on the
    /// caller: for the runner it is fatal, because transcribing a corpus
    /// unbiased produces work that has to be redone; for the live tier it is
    /// not, because a live turn is superseded within the hour either way.
    ///
    /// # Errors
    /// If the API is unreachable or answers something unreadable.
    pub fn prompt(&self, api_base: &str) -> Result<Option<String>, Error> {
        let url = format!("{}/sync/vocabulary/prompt", api_base.trim_end_matches('/'));
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
/// ⚠ **`snake_case` on the wire, and NOT by omission.** The route was written for
/// pydantic, which serialises field names as declared, so `asr_model` is the
/// name the server matches. A `rename_all = "camelCase"` here — the reflex,
/// since the browsing plane's types all carry one — makes every push a 422 the
/// agent logs and shrugs at, which is the instant feed off with nothing broken.
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
