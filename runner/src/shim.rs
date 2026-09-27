//! Driving a model shim: a long-lived child speaking JSON over stdio.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

#[derive(Debug)]
pub enum Error {
    Spawn(String),
    Write(String),
    /// The child closed its stdout: it died, and the caller must respawn it.
    Closed,
    Protocol(String),
    /// The shim answered `ok: false`. The job failed but the shim is fine, so
    /// respawning it would waste a model load to reach the same answer.
    Refused(String),
}

/// Describe a reply that would not parse, without reproducing it.
///
/// A parse error alone cannot tell apart a bare `NaN` from Python's
/// `json.dumps`, a C-level write to fd 1 under the shim's stdout guard, and a
/// truncated line, and each needs a different fix.
///
/// ⚠ Never log the reply itself: it carries a transcript of private speech.
/// Only its shape is reported: length, byte classes, bare literals and whether
/// it ends in a brace.
fn shape_of(response: &str) -> String {
    let bytes = response.as_bytes();
    let len = bytes.len();
    let ends_brace = response.trim_end().ends_with('}');
    // Bare `NaN`/`Infinity`: Python emits them, JSON does not allow them.
    let literal = ["NaN", "Infinity", "-Infinity"]
        .into_iter()
        .find(|t| response.contains(t))
        .unwrap_or("none");
    // Unescaped control bytes mean something other than the protocol wrote to
    // the stream.
    let control = bytes
        .iter()
        .filter(|b| **b < 0x20 && **b != b'\n' && **b != b'\r')
        .count();
    let non_ascii = bytes.iter().filter(|b| **b >= 0x80).count();
    format!(
        "reply len={len} ends_with_brace={ends_brace} bare_literal={literal} \
         control_bytes={control} non_ascii={non_ascii}"
    )
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

/// A running shim (`recall.shim`), held for the life of the runner because
/// loading a model costs seconds.
pub struct Shim {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Shim {
    /// Start `program args…` as a shim.
    ///
    /// stderr is inherited: the shim logs there, and that output must not be
    /// read as protocol.
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
        let parsed: serde_json::Value = serde_json::from_str(&response)
            .map_err(|e| Error::Protocol(format!("{e}; {}", shape_of(&response))))?;
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

    /// Ask the shim what it is. Answered by the protocol layer, so it works
    /// even when the model failed to load, which makes it safe for capability
    /// discovery at startup.
    ///
    /// # Errors
    /// Whatever `request` reports, or `Protocol` if the answer has no name.
    pub fn hello(&mut self) -> Result<String, Error> {
        let answer = self.request("hello", &serde_json::json!({}))?;
        answer
            .get("shim")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| Error::Protocol("hello did not name the shim".to_owned()))
    }

    /// Diarize one clip, and embed each speaker found in it.
    ///
    /// Embedding happens in the same request, because the model is on this
    /// machine; without it a diarized turn reaches the archive as a bare
    /// `SPEAKER_00` with no name guess.
    ///
    /// No tuning is passed: the archive is diarized with the shipped pyannote
    /// parameters.
    ///
    /// # Errors
    /// Whatever `request` reports.
    pub fn diarize(&mut self, audio: &Path) -> Result<serde_json::Value, Error> {
        self.request(
            "diarize",
            &serde_json::json!({ "audio": audio.to_string_lossy(), "embed": true }),
        )
    }

    /// Embed one stretch of a clip into the vector that names a voice.
    ///
    /// A span, not the whole clip: enrolment learns one voice from one labelled
    /// turn, and the rest of the clip holds other speakers. The shim does the
    /// cutting, since it already decodes the audio.
    ///
    /// # Errors
    /// Whatever `request` reports.
    pub fn embed(
        &mut self,
        audio: &Path,
        start_s: f64,
        end_s: f64,
    ) -> Result<serde_json::Value, Error> {
        self.request(
            "embed",
            &serde_json::json!({
                "audio": audio.to_string_lossy(),
                "start": start_s,
                "end": end_s,
            }),
        )
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
        // Kill and reap the child so it does not outlive the runner.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
