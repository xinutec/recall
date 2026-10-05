//! Driving a model shim: a long-lived child speaking JSON over stdio.

use audiocore::shim::{Hello, Response, asr, voices};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

#[derive(Debug)]
pub enum Error {
    Spawn(String),
    Write(String),
    /// The child closed its stdout: it died, and must be respawned.
    Closed,
    Protocol(String),
    /// The shim answered `ok: false`: the job failed, the shim is fine.
    Refused(String),
}

/// Describe a reply that would not parse, without quoting it: it carries
/// private speech.
///
/// The shape tells apart what a parse error alone cannot: a bare `NaN` from
/// Python's `json.dumps`, a C library writing to fd 1, a truncated line.
fn shape_of(response: &str) -> String {
    let bytes = response.as_bytes();
    let len = bytes.len();
    let ends_brace = response.trim_end().ends_with('}');
    let literal = ["NaN", "Infinity", "-Infinity"]
        .into_iter()
        .find(|t| response.contains(t))
        .unwrap_or("none");
    // JSON escapes control bytes; a raw one came from another writer.
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
    /// Start `program args…` as a shim. Its stderr, where it logs, is inherited.
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
        let parsed: Response = serde_json::from_str(&response)
            .map_err(|e| Error::Protocol(format!("{e}; {}", shape_of(&response))))?;
        if !parsed.ok {
            let why = parsed.error.unwrap_or_else(|| "no reason given".to_owned());
            return Err(Error::Refused(why));
        }
        Ok(parsed.result.unwrap_or(serde_json::Value::Null))
    }

    /// Ask the shim its name. The protocol layer answers, so this works even
    /// when the model failed to load.
    ///
    /// # Errors
    /// Whatever `request` reports, or `Protocol` if the answer has no name.
    pub fn hello(&mut self) -> Result<String, Error> {
        let answer = self.request("hello", &serde_json::json!({}))?;
        serde_json::from_value::<Hello>(answer)
            .map(|hello| hello.shim)
            .map_err(|_| Error::Protocol("hello did not name the shim".to_owned()))
    }

    /// Send a typed request and read a typed reply, keeping the raw reply too:
    /// that is what gets stored.
    ///
    /// # Errors
    /// Whatever `request` reports, or `Refused` for a reply that does not read
    /// as `T` (the contract in `audiocore::shim`).
    pub fn ask<Q: Serialize, T: DeserializeOwned>(
        &mut self,
        op: &str,
        request: &Q,
    ) -> Result<Answer<T>, Error> {
        let args = serde_json::to_value(request).map_err(|e| Error::Write(e.to_string()))?;
        let raw = self.request(op, &args)?;
        let reply = T::deserialize(&raw)
            .map_err(|e| Error::Refused(format!("{op} reply outside the contract: {e}")))?;
        Ok(Answer { raw, reply })
    }

    /// Diarize one clip and embed each speaker found, in one request: the
    /// embeddings are what let recalld guess a name. Pyannote's shipped
    /// parameters, untuned.
    ///
    /// # Errors
    /// Whatever [`Shim::ask`] reports.
    pub fn diarize(&mut self, audio: &Path) -> Result<Answer<voices::Diarization>, Error> {
        let request = voices::Diarize {
            audio: audio.to_string_lossy().into_owned(),
            embed: true,
        };
        self.ask(voices::DIARIZE, &request)
    }

    /// Embed one stretch of a clip, in seconds from its start. A stretch, since
    /// the rest of the clip holds other voices; the shim cuts it.
    ///
    /// # Errors
    /// Whatever [`Shim::ask`] reports.
    pub fn embed(
        &mut self,
        audio: &Path,
        start_s: f64,
        end_s: f64,
    ) -> Result<Answer<voices::Embedding>, Error> {
        let request = voices::Embed {
            audio: audio.to_string_lossy().into_owned(),
            start: start_s,
            end: end_s,
        };
        self.ask(voices::EMBED, &request)
    }

    /// Transcribe one clip.
    ///
    /// # Errors
    /// Whatever [`Shim::ask`] reports.
    pub fn transcribe(
        &mut self,
        audio: &Path,
        model: Option<&str>,
        initial_prompt: Option<&str>,
    ) -> Result<Answer<asr::Reply>, Error> {
        let request = asr::Request {
            audio: audio.to_string_lossy().into_owned(),
            words: true,
            model: model.map(str::to_owned),
            language: None,
            initial_prompt: initial_prompt.map(str::to_owned),
        };
        self.ask(asr::OP, &request)
    }

    /// Transcribe one clip with the default model, in `language` (`"nl"`) or,
    /// given `None`, whatever the model detects.
    ///
    /// # Errors
    /// Whatever [`Shim::ask`] reports.
    pub fn transcribe_in(
        &mut self,
        audio: &Path,
        language: Option<&str>,
        initial_prompt: Option<&str>,
    ) -> Result<Answer<asr::Reply>, Error> {
        let request = asr::Request {
            audio: audio.to_string_lossy().into_owned(),
            words: true,
            model: None,
            language: language.map(str::to_owned),
            initial_prompt: initial_prompt.map(str::to_owned),
        };
        self.ask(asr::OP, &request)
    }
}

/// A shim's reply, raw (what recalld stores) and typed.
#[derive(Debug, Clone)]
pub struct Answer<T> {
    pub raw: serde_json::Value,
    pub reply: T,
}

impl Drop for Shim {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
