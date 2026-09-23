//! The browsing API, as a person at a terminal sees it.
//!
//! This reads the system of record, the fleet, never a local copy that could
//! diverge from it.
//!
//! Every type here mirrors `recalld::reads` field for field. They are separate
//! declarations on purpose: a client compiled against the server's structs
//! cannot detect when the server changes its HTTP contract.

use serde::Deserialize;

/// One turn, in the shape `recalld::reads::TranscriptOut` serialises.
///
/// There is no `provenance` or `superseded_by`: the route sends `tier` as the
/// summary instead (see [`crate::render::details`]).
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

/// What `/api/capture` says about the recorders, verbatim.
///
/// Nothing in this crate writes the pause; this type only reports it.
// Four bools, because the route sends four: `running` is what the recorders
// are doing, `desired_running` what they were asked to do, and `settled`
// whether those agree. What a combination means is the server's call.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capture {
    pub running: bool,
    pub paused_until: Option<String>,
    pub desired_running: bool,
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
    /// Human-confirmed names only. The route excludes voiceprint guesses: on
    /// unfamiliar audio a stranger can score 0.95 against an enrolled voice.
    pub speakers: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Sessions {
    pub items: Vec<Session>,
}

/// One speaker's run of consecutive turns, merged, as the export sends it.
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

/// The several microphones that heard one utterance, folded into one card.
#[derive(Debug, Deserialize)]
pub struct Moment {
    pub start: String,
    pub end: String,
    /// The best mic's version — what a reader should read.
    pub primary: Vec<Turn>,
    /// The other mics' overlapping versions, for comparison.
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
    /// The fleet could not be reached, or answered a status.
    Http(String),
    /// It answered, and the body was not what this understands.
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

/// Where a saved browsing session lives, under `$HOME`.
///
/// ⚠ The CLI needs a credential, not an exemption: adding the transcript
/// routes to `recalld::webauth`'s `DEVICE_EXEMPT` would open the archive to
/// every device token on the network.
pub const SESSION_FILE: &str = ".config/recall/session";

/// What to tell someone whose request was refused for want of a session.
pub const HOW_TO_SIGN_IN: &str = concat!(
    "no session — sign in at http://10.100.0.2:8000/ in a browser, then save the
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

/// A reachable recall API, and the session it presents.
pub struct Api {
    base: String,
    /// The `recall_session` cookie, if any. `None` still works for the
    /// device-exempt routes (`sources`, `capture`), so a missing session is not
    /// an error until a request is refused.
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

    /// The session from the environment, else the saved file. Absent is fine.
    ///
    /// Whitespace is trimmed: a shell redirect usually leaves a newline, and a
    /// cookie with one is rejected as a bad signature.
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

    /// Turn a transport failure into one that says what to do about it. A 401
    /// means no session or an invalid one, so it carries sign-in instructions.
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

    /// Full-text search across the archive.
    ///
    /// # Errors
    /// If the fleet is unreachable or answers something unreadable.
    pub fn search(&self, query: &str, limit: i64) -> Result<Vec<Turn>, Error> {
        let items: Items =
            self.get(&format!("/api/search?q={}&limit={limit}", urlencode(query)))?;
        Ok(items.items)
    }

    /// Specific turns by id.
    ///
    /// # Errors
    /// As [`Api::search`]. A non-integer id is refused by the route, not here.
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

    /// The turns the model was least sure of — what a human should look at.
    ///
    /// # Errors
    /// As [`Api::search`].
    pub fn review(&self, limit: i64) -> Result<Vec<Turn>, Error> {
        let items: Items = self.get(&format!("/api/review?limit={limit}"))?;
        Ok(items.items)
    }

    /// Every recorder the fleet knows.
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

    /// Every turn one source has, with its id — what a correction needs.
    ///
    /// Both `primary` and `alternates` are flattened. The split ranks the
    /// microphones that heard one moment, so for one source this does not
    /// double-count, and `primary` alone would hide turns that lost to another
    /// mic.
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
            .flat_map(|m| m.primary.into_iter().chain(m.alternates))
            .collect();
        turns.sort_by(|a, b| a.start.cmp(&b.start).then(a.id.cmp(&b.id)));
        Ok(turns)
    }

    /// A window of the always-on stream, split at the silences and folded per
    /// moment.
    ///
    /// `after` and `before` are required here, although the route accepts
    /// neither: without a window this would be the newest page of the whole
    /// archive, which `timeline` already answers.
    ///
    /// # Errors
    /// As [`Api::search`]. A malformed instant is a 400 from the route, which
    /// arrives as [`Error::Http`].
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

    /// Replace a turn's text with a person's own words.
    ///
    /// The one write in this crate. It reaches the corrections corpus, the only
    /// part of the archive not re-derivable from audio; `recall-cli correct`
    /// requires `--apply` and prints the change first.
    ///
    /// # Errors
    /// As [`Api::search`]. A 400 arrives as [`Error::Http`] carrying the route's
    /// own explanation of what was wrong.
    pub fn correct(&self, id: i64, text: &str) -> Result<i64, Error> {
        let response = self
            .signed(self.agent.post(&format!("{}/api/correct", self.base)))
            .send_json(serde_json::json!({ "id": id, "text": text }))
            .map_err(|e| self.refused(&e))?;
        let body: NewId = serde_json::from_reader(response.into_reader())
            .map_err(|e| Error::Body(e.to_string()))?;
        Ok(body.new_id)
    }
}

/// Percent-encode a query-string value: everything but RFC 3986 unreserved
/// characters is escaped. Hand-written to avoid a dependency for one function.
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
