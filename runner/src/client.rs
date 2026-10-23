//! Talking to recalld: lease, fetch, ack.

use audiocore::work::{Job, Lease, LiveStored, LiveTurn, LiveTurns, Prompt};
use std::io::Read;
use std::path::Path;

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
        let body: Lease = serde_json::from_reader(response.into_reader())
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

    /// Hold a job this runner is still working on for another lease's length.
    /// `false` when the server says it is finished, or has no such route (the
    /// messages queue): either way there is nothing to hold.
    ///
    /// # Errors
    /// If recalld refuses otherwise or is unreachable.
    pub fn renew(&self, id: i64) -> Result<bool, Error> {
        let url = format!("{}/work/v1/jobs/{id}/renew", self.base);
        match self.auth(self.agent.put(&url)).call() {
            Ok(_) => Ok(true),
            Err(ureq::Error::Status(404 | 405, _)) => Ok(false),
            Err(err) => Err(Error::Http(err.to_string())),
        }
    }

    /// Push provisional live turns. Lossy on purpose: the archive pass
    /// supersedes them.
    ///
    /// # Errors
    /// If recalld refuses or is unreachable.
    pub fn push_live(&self, turns: &[LiveTurn]) -> Result<usize, Error> {
        let response = self
            .auth(self.agent.post(&format!("{}/sync/live", self.base)))
            .send_json(LiveTurns {
                turns: turns.to_vec(),
            })
            .map_err(|e| Error::Http(e.to_string()))?;
        let body: LiveStored = serde_json::from_reader(response.into_reader())
            .map_err(|e| Error::Body(e.to_string()))?;
        Ok(body.stored)
    }

    /// Whisper's `initial_prompt` for `source`'s recordings: its owner's names
    /// and glossary, fetched per job so new ones reach queued jobs. `Ok(None)`:
    /// empty, no biasing.
    ///
    /// # Errors
    /// If recalld is unreachable or answers something unreadable.
    pub fn prompt(&self, source: &str) -> Result<Option<String>, Error> {
        // A source id is one path-safe word (`transcript::SourceId`).
        let url = format!("{}/sync/vocabulary/prompt?source={source}", self.base);
        let response = self
            .auth(self.agent.get(&url))
            .call()
            .map_err(|e| Error::Http(e.to_string()))?;
        let body: Prompt = serde_json::from_reader(response.into_reader())
            .map_err(|e| Error::Body(e.to_string()))?;
        Ok(body.prompt.filter(|p| !p.trim().is_empty()))
    }
}
