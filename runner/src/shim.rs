//! Driving a model shim: a long-lived child speaking JSON over stdio.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

#[derive(Debug)]
pub enum Error {
    Spawn(String),
    Write(String),
    /// The child closed its stdout — it died, and the caller must respawn
    /// rather than keep writing into a pipe nobody reads.
    Closed,
    Protocol(String),
    /// The shim answered, and the answer was "no". Distinct from the transport
    /// errors above: this job failed, the SHIM is fine, and retrying it against
    /// a fresh process would waste a model load to reach the same answer.
    Refused(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "cannot start shim: {e}"),
            Self::Write(e) => write!(f, "cannot write to shim: {e}"),
            Self::Closed => write!(f, "shim closed its output"),
            Self::Protocol(e) => write!(f, "shim protocol: {e}"),
            Self::Refused(e) => write!(f, "shim refused the job: {e}"),
        }
    }
}

impl std::error::Error for Error {}

/// A running shim. Held for the life of the runner: loading a Whisper model
/// costs seconds, and paying that per clip is the bill this architecture stops
/// paying (`recall.shim`).
pub struct Shim {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Shim {
    /// Start `program args…` as a shim.
    ///
    /// ⚠ stderr is INHERITED on purpose: the shim logs there, and its model
    /// chatter is exactly what must not be captured as if it were protocol.
    ///
    /// # Errors
    /// If the process cannot be started or its pipes cannot be taken.
    pub fn spawn(program: &str, args: &[String]) -> Result<Self, Error> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| Error::Spawn(e.to_string()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::Spawn("no stdin".to_owned()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Spawn("no stdout".to_owned()))?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
        })
    }

    /// Send one request and read its response.
    ///
    /// # Errors
    /// Transport failures, or `Refused` when the shim answered `ok: false`.
    pub fn request(
        &mut self,
        op: &str,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value, Error> {
        let id = self.next_id;
        self.next_id += 1;
        let mut message = args.clone();
        let object = message
            .as_object_mut()
            .ok_or_else(|| Error::Protocol("args must be an object".to_owned()))?;
        object.insert("id".to_owned(), serde_json::json!(id.to_string()));
        object.insert("op".to_owned(), serde_json::json!(op));
        let line = serde_json::to_string(&message).map_err(|e| Error::Write(e.to_string()))?;
        writeln!(self.stdin, "{line}").map_err(|e| Error::Write(e.to_string()))?;
        self.stdin
            .flush()
            .map_err(|e| Error::Write(e.to_string()))?;

        let mut response = String::new();
        let read = self
            .stdout
            .read_line(&mut response)
            .map_err(|e| Error::Protocol(e.to_string()))?;
        if read == 0 {
            return Err(Error::Closed);
        }
        let parsed: serde_json::Value =
            serde_json::from_str(&response).map_err(|e| Error::Protocol(e.to_string()))?;
        if parsed.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
            let why = parsed
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("no reason given");
            return Err(Error::Refused(why.to_owned()));
        }
        Ok(parsed
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null))
    }

    /// Transcribe one clip.
    ///
    /// # Errors
    /// Whatever `request` reports.
    pub fn transcribe(
        &mut self,
        audio: &Path,
        model: Option<&str>,
        initial_prompt: Option<&str>,
    ) -> Result<serde_json::Value, Error> {
        let mut args = serde_json::json!({
            "audio": audio.to_string_lossy(),
            "words": true,
        });
        if let Some(model) = model {
            args["model"] = serde_json::json!(model);
        }
        if let Some(prompt) = initial_prompt {
            args["initial_prompt"] = serde_json::json!(prompt);
        }
        self.request("transcribe", &args)
    }
}

impl Drop for Shim {
    fn drop(&mut self) {
        // Closing stdin is how a shim is asked to stop: its loop ends when
        // stdin does. Kill only if it will not take the hint.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
