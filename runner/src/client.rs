//! Talking to recalld: lease, fetch, ack.

use serde::Deserialize;
use std::io::Read;
use std::path::Path;

/// One unit of work, exactly as the queue hands it over.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Job {
    pub id: i64,
    pub kind: String,
    /// The blob to work on, under the `room` source.
    pub filename: String,
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

    /// Take the next job, or `None` when the queue is empty.
    ///
    /// # Errors
    /// If recalld is unreachable or answers something unreadable.
    pub fn lease(&self) -> Result<Option<Job>, Error> {
        let response = self
            .auth(self.agent.put(&format!("{}/work/v1/lease", self.base)))
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
}

/// The household glossary, as Whisper's `initial_prompt`.
///
/// Lives on the API's SYNC plane rather than recalld, because the vocabulary is
/// in the MEANING store and recalld owns the audio plane. It moves to recalld
/// with everything else at stage F.
///
/// ⚠ Read ONCE at startup and carried on every job. The shim must not fetch it
/// — a model process holds no database (stage E2) — and writing it onto each job
/// at derivation time would pin it, so a name learned today would never reach a
/// job queued yesterday.
///
/// `Ok(None)` means the vocabulary is EMPTY, which is fine and means "no
/// biasing". Failing to reach it is an error, and the caller should treat it as
/// fatal: transcribing a corpus without the biasing it was built for produces
/// work that has to be redone.
///
/// # Errors
/// If the API is unreachable or answers something unreadable.
pub fn fetch_prompt(api_base: &str, token: &str) -> Result<Option<String>, Error> {
    let url = format!("{}/sync/vocabulary/prompt", api_base.trim_end_matches('/'));
    let response = ureq::get(&url)
        .set("authorization", &format!("Bearer {token}"))
        .call()
        .map_err(|e| Error::Http(e.to_string()))?;
    let body: PromptBody =
        serde_json::from_reader(response.into_reader()).map_err(|e| Error::Body(e.to_string()))?;
    Ok(body.prompt.filter(|p| !p.trim().is_empty()))
}
