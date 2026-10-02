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
        let parsed: Response = serde_json::from_str(&response)
            .map_err(|e| Error::Protocol(format!("{e}; {}", shape_of(&response))))?;
        if !parsed.ok {
            let why = parsed.error.unwrap_or_else(|| "no reason given".to_owned());
            return Err(Error::Refused(why));
        }
        Ok(parsed.result.unwrap_or(serde_json::Value::Null))
    }

    /// Ask the shim what it is. Answered by the protocol layer, so it works
    /// even when the model failed to load, which makes it safe for capability
    /// discovery at startup.
    ///
    /// # Errors
    /// Whatever `request` reports, or `Protocol` if the answer has no name.
    pub fn hello(&mut self) -> Result<String, Error> {
        let answer = self.request("hello", &serde_json::json!({}))?;
        serde_json::from_value::<Hello>(answer)
            .map(|hello| hello.shim)
            .map_err(|_| Error::Protocol("hello did not name the shim".to_owned()))
    }

    /// Send a typed request and read a typed reply, keeping the reply as it
    /// came: the runner stores what the shim said, not its reading of it.
    ///
    /// A reply that does not read as `T` is the shim breaking the contract
    /// (`audiocore::shim`), and is refused like any answer the job cannot use.
    ///
    /// # Errors
    /// Whatever `request` reports, or `Refused` for a reply outside the contract.
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
    /// Whatever [`Shim::ask`] reports.
    pub fn diarize(&mut self, audio: &Path) -> Result<Answer<voices::Diarization>, Error> {
        let request = voices::Diarize {
            audio: audio.to_string_lossy().into_owned(),
            embed: true,
        };
        self.ask(voices::DIARIZE, &request)
    }

    /// Embed one stretch of a clip into the vector that names a voice.
    ///
    /// A span, not the whole clip: enrolment learns one voice from one labelled
    /// turn, and the rest of the clip holds other speakers. The shim does the
    /// cutting, since it already decodes the audio.
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

    /// The language spoken in one clip's first 30 s, without transcribing it.
    ///
    /// # Errors
    /// Whatever [`Shim::ask`] reports.
    pub fn detect_language(&mut self, audio: &Path) -> Result<Answer<asr::Detected>, Error> {
        let request = asr::Detect {
            audio: audio.to_string_lossy().into_owned(),
            model: None,
        };
        self.ask(asr::DETECT, &request)
    }

    /// Transcribe one clip in a stated `language` (`"nl"`), the default model;
    /// `None` leaves detection to the model, as [`Shim::transcribe`] does.
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

/// A shim's reply, as it came and as read.
#[derive(Debug, Clone)]
pub struct Answer<T> {
    /// What the shim sent: what the fleet stores.
    pub raw: serde_json::Value,
    pub reply: T,
}

impl Drop for Shim {
    fn drop(&mut self) {
        // Kill and reap the child so it does not outlive the runner.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
