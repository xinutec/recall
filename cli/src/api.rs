//! The browsing API's client.
//!
//! The types mirror `recalld::reads` field for field, declared separately: a
//! client compiled against the server's own structs could not notice the HTTP
//! contract changing.

use serde::Deserialize;

/// One turn, as `recalld::reads::TranscriptOut` serialises it.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub id: i64,
    pub start: String,
    pub end: String,
    pub text: String,
    pub language: Option<String>,
    pub speaker: Option<String>,
    pub speaker_confirmed: bool,
    pub speaker_confidence: Option<f64>,
    pub confidence: Option<f64>,
    pub loudness: Option<f64>,
    pub model: Option<String>,
    pub tier: String,
    pub hidden: Option<String>,
    pub source: Option<String>,
    pub cluster: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Items {
    pub items: Vec<Turn>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    pub items: Vec<Turn>,
    pub has_more: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub active: bool,
    pub last_active: Option<String>,
    pub recording: bool,
}

#[derive(Debug, Deserialize)]
pub struct Sources {
    pub items: Vec<Source>,
}

/// What `/api/capture` says about the recorders. Read-only: this crate never
/// pauses capture.
#[expect(clippy::struct_excessive_bools, reason = "the route sends these four")]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capture {
    pub running: bool,
    pub paused_until: Option<String>,
    /// What the recorders were asked to do.
    pub desired_running: bool,
    /// Whether `running` agrees with `desired_running`.
    pub settled: bool,
    pub mic_reachable: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    pub title: String,
    pub start: String,
    pub end: String,
    pub turn_count: i64,
    /// Confirmed names only: on unfamiliar audio a stranger can score 0.95
    /// against an enrolled voice.
    pub speakers: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Sessions {
    pub items: Vec<Session>,
}

/// One speaker's consecutive turns, merged.
#[derive(Debug, Deserialize)]
pub struct Bubble {
    pub start: String,
    pub speaker: String,
    pub text: String,
}

#[derive(Debug, Deserialize)]
pub struct Export {
    pub session: String,
    pub date: Option<String>,
    pub speakers: Vec<String>,
    pub turns: Vec<Bubble>,
}

/// One shown line, with the other microphones' versions of it.
#[derive(Debug, Deserialize)]
pub struct Moment {
    pub start: String,
    pub end: String,
    /// The best mic's version.
    pub primary: Turn,
    pub alternates: Vec<Turn>,
    pub sources: Vec<String>,
}

/// A run of turns with no long silence in it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    pub start: String,
    pub end: String,
    pub turn_count: usize,
    pub speakers: Vec<String>,
    pub preview: String,
    pub moments: Vec<Moment>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversations {
    pub items: Vec<Conversation>,
    pub has_more: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewId {
    pub new_id: i64,
}

#[derive(Debug)]
pub enum Error {
    /// Unreachable, or an error status.
    Http(String),
    /// A body this cannot read.
    Body(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(e) => write!(f, "recall api: {e}"),
            Self::Body(e) => write!(f, "unreadable response: {e}"),
        }
    }
}

impl std::error::Error for Error {}

/// Clips per re-transcription request: `recalld::retranscribe::MAX_PER_REQUEST`,
/// held equal by a test.
pub const RETRANSCRIBE_BATCH: usize = 1000;

#[derive(Debug, Deserialize)]
pub struct Requested {
    pub queued: Vec<String>,
    /// No finished transcription to redo.
    pub skipped: Vec<String>,
}

/// A clip that lost speech to a repetition loop.
#[derive(Debug, Deserialize)]
pub struct Candidate {
    pub filename: String,
    pub looped_speech_s: f64,
}

/// The saved browsing session, under `$HOME`. The transcript routes need one;
/// exempting them for device tokens would open the archive to every device.
pub const SESSION_FILE: &str = ".config/recall/session";

pub const HOW_TO_SIGN_IN: &str = concat!(
    "no session — sign in at https://recall.xinutec.org/ in a browser, then save the
",
    "value of the `recall_session` cookie:
",
    "
",
    "    umask 077; printf %s '<cookie value>' > ~/.config/recall/session
",
    "
",
    "or pass it as RECALL_SESSION in the environment. It is valid for seven days."
);

pub struct Api {
    base: String,
    /// The `recall_session` cookie. `None` still works for `sources` and
    /// `capture`, so its absence is an error only when a request is refused.
    session: Option<String>,
    agent: ureq::Agent,
}

impl Api {
    #[must_use]
    pub fn new(base: &str, session: Option<String>) -> Self {
        Self {
            base: base.trim_end_matches('/').to_owned(),
            session,
            agent: ureq::AgentBuilder::new().build(),
        }
    }

    /// `RECALL_SESSION`, else [`SESSION_FILE`]. Trimmed: a trailing newline
    /// fails the signature.
    #[must_use]
    pub fn saved_session() -> Option<String> {
        if let Ok(from_env) = std::env::var("RECALL_SESSION")
            && !from_env.trim().is_empty()
        {
            return Some(from_env.trim().to_owned());
        }
        let home = std::env::var_os("HOME")?;
        let raw = std::fs::read_to_string(std::path::Path::new(&home).join(SESSION_FILE)).ok()?;
        let trimmed = raw.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    }

    fn signed(&self, req: ureq::Request) -> ureq::Request {
        match &self.session {
            Some(cookie) => req.set("cookie", &format!("recall_session={cookie}")),
            None => req,
        }
    }

    /// A transport error; a 401 carries sign-in instructions.
    fn refused(&self, err: &ureq::Error) -> Error {
        if matches!(err, ureq::Error::Status(401, _)) {
            let why = if self.session.is_some() {
                "session rejected (expired, or minted with a different secret)"
            } else {
                "not signed in"
            };
            return Error::Http(format!("{why}\n\n{HOW_TO_SIGN_IN}"));
        }
        Error::Http(err.to_string())
    }

    fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, Error> {
        let response = self
            .signed(self.agent.get(&format!("{}{path}", self.base)))
            .call()
            .map_err(|e| self.refused(&e))?;
        serde_json::from_reader(response.into_reader()).map_err(|e| Error::Body(e.to_string()))
    }

    /// Full-text search.
    ///
    /// # Errors
    /// If recalld is unreachable or answers something unreadable.
    pub fn search(&self, query: &str, limit: i64) -> Result<Vec<Turn>, Error> {
        let items: Items =
            self.get(&format!("/api/search?q={}&limit={limit}", urlencode(query)))?;
        Ok(items.items)
    }

    /// Specific turns by id.
    ///
    /// # Errors
    /// As [`Api::search`].
    pub fn transcripts(&self, ids: &[i64]) -> Result<Vec<Turn>, Error> {
        let joined = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        let items: Items = self.get(&format!("/api/transcripts?ids={joined}"))?;
        Ok(items.items)
    }

    /// The newest turns, or the page before `before`.
    ///
    /// # Errors
    /// As [`Api::search`].
    pub fn timeline(&self, limit: i64, before: Option<&str>) -> Result<Page, Error> {
        use std::fmt::Write as _;
        let mut path = format!("/api/timeline?limit={limit}");
        if let Some(before) = before {
            let _ = write!(path, "&before={}", urlencode(before));
        }
        self.get(&path)
    }

    /// The turns the model was least sure of.
    ///
    /// # Errors
    /// As [`Api::search`].
    pub fn review(&self, limit: i64) -> Result<Vec<Turn>, Error> {
        let items: Items = self.get(&format!("/api/review?limit={limit}"))?;
        Ok(items.items)
    }

    /// Every recorder.
    ///
    /// # Errors
    /// As [`Api::search`].
    pub fn sources(&self) -> Result<Vec<Source>, Error> {
        let sources: Sources = self.get("/api/sources")?;
        Ok(sources.items)
    }

    /// Whether the recorders are running, and until when they are not.
    ///
    /// # Errors
    /// As [`Api::search`].
    pub fn capture(&self) -> Result<Capture, Error> {
        self.get("/api/capture")
    }

    /// Every uploaded session, newest first.
    ///
    /// # Errors
    /// As [`Api::search`].
    pub fn sessions(&self) -> Result<Vec<Session>, Error> {
        let sessions: Sessions = self.get("/api/sessions")?;
        Ok(sessions.items)
    }

    /// One session's clean transcript, consecutive same-speaker turns merged.
    ///
    /// # Errors
    /// As [`Api::search`].
    pub fn session_transcript(&self, source: &str) -> Result<Export, Error> {
        self.get(&format!("/api/sessions/{}/transcript", urlencode(source)))
    }

    /// Every turn one source has, with ids for correcting.
    ///
    /// Alternates included: filtered to one source there is no double
    /// counting, and `primary` alone would drop turns another mic won.
    ///
    /// # Errors
    /// As [`Api::search`].
    pub fn source_turns(&self, source: &str, limit: i64) -> Result<Vec<Turn>, Error> {
        let found: Conversations = self.get(&format!(
            "/api/conversations?source={}&limit={limit}&gap={}",
            urlencode(source),
            f64::MAX
        ))?;
        let mut turns: Vec<Turn> = found
            .items
            .into_iter()
            .flat_map(|c| c.moments)
            .flat_map(|m| std::iter::once(m.primary).chain(m.alternates))
            .collect();
        turns.sort_by(|a, b| a.start.cmp(&b.start).then(a.id.cmp(&b.id)));
        Ok(turns)
    }

    /// A window of the stream, split at silences longer than `gap` seconds.
    /// The route's `after` and `before` are optional; without them this is
    /// `timeline`.
    ///
    /// # Errors
    /// As [`Api::search`].
    pub fn conversations(
        &self,
        after: &str,
        before: &str,
        gap: f64,
        limit: i64,
    ) -> Result<Conversations, Error> {
        self.get(&format!(
            "/api/conversations?after={}&before={}&gap={gap}&limit={limit}",
            urlencode(after),
            urlencode(before)
        ))
    }

    /// Replace a turn's text with a person's own words, which no rerun of the
    /// models can recover.
    ///
    /// # Errors
    /// As [`Api::search`]; a 400 carries the route's reason.
    pub fn correct(&self, id: i64, text: &str) -> Result<i64, Error> {
        let response = self
            .signed(self.agent.post(&format!("{}/api/correct", self.base)))
            .send_json(serde_json::json!({ "id": id, "text": text }))
            .map_err(|e| self.refused(&e))?;
        let body: NewId = serde_json::from_reader(response.into_reader())
            .map_err(|e| Error::Body(e.to_string()))?;
        Ok(body.new_id)
    }

    /// Say nobody spoke: the turn is hidden and its words filed as invented.
    ///
    /// # Errors
    /// As [`Api::correct`].
    pub fn no_speech(&self, id: i64) -> Result<(), Error> {
        self.signed(self.agent.post(&format!("{}/api/no-speech", self.base)))
            .send_json(serde_json::json!({ "id": id }))
            .map_err(|e| self.refused(&e))?;
        Ok(())
    }

    /// Queue clips for transcription again. When the new words land, the
    /// machine lines are set aside and a person's stay.
    ///
    /// # Errors
    /// As [`Api::correct`]; more than a batch is a 400.
    pub fn retranscribe(&self, filenames: &[String]) -> Result<Requested, Error> {
        let response = self
            .signed(self.agent.post(&format!("{}/api/retranscribe", self.base)))
            .send_json(serde_json::json!({ "filenames": filenames }))
            .map_err(|e| self.refused(&e))?;
        serde_json::from_reader(response.into_reader()).map_err(|e| Error::Body(e.to_string()))
    }

    /// Clips whose stored transcription lost more than `min_speech_s` of
    /// measured speech to a repetition loop, most lost first.
    ///
    /// # Errors
    /// As [`Api::correct`].
    pub fn retranscribe_candidates(&self, min_speech_s: f64) -> Result<Vec<Candidate>, Error> {
        self.get(&format!("/api/retranscribe?min_speech_s={min_speech_s}"))
    }

    /// Take back [`Api::retranscribe`] for one clip: `cancelled` if it was
    /// still waiting, `restored` if its old lines show again.
    ///
    /// # Errors
    /// As [`Api::correct`]; a clip with nothing set aside is a 400.
    pub fn undo_retranscribe(&self, filename: &str) -> Result<String, Error> {
        let response = self
            .signed(
                self.agent
                    .post(&format!("{}/api/retranscribe/undo", self.base)),
            )
            .send_json(serde_json::json!({ "filename": filename }))
            .map_err(|e| self.refused(&e))?;
        serde_json::from_reader(response.into_reader()).map_err(|e| Error::Body(e.to_string()))
    }

    /// Take back [`Api::no_speech`]: the turn shows again.
    ///
    /// # Errors
    /// As [`Api::correct`]; a turn not hidden as nobody spoke is a 400.
    pub fn undo_no_speech(&self, id: i64) -> Result<(), Error> {
        self.signed(
            self.agent
                .post(&format!("{}/api/no-speech/undo", self.base)),
        )
        .send_json(serde_json::json!({ "id": id }))
        .map_err(|e| self.refused(&e))?;
        Ok(())
    }
}

/// Percent-encode all but RFC 3986 unreserved characters.
fn urlencode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            _ => {
                use std::fmt::Write as _;
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}
