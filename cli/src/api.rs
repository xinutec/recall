//! The browsing API, as a person at a terminal sees it.
//!
//! ⚠ **This reads the SYSTEM OF RECORD, which is the fleet — not the Mac.**
//! `recall.cli` opened the Mac's `recall.sqlite` directly, which was right while
//! the Mac was the record and has been wrong since Isis became it
//! (`deploy/hm-agents.nix`: "10.100.0.2:8000 is the system of record"). The
//! local file still exists and still receives writes from `refine`, so a CLI
//! reading it answers about a diverging copy — measured 2026-09-15, its newest
//! turn was five days old. Going through the API cannot diverge by
//! construction.
//!
//! Every type here mirrors `recalld::reads` field for field. They are separate
//! declarations rather than a shared crate on purpose: this is a client of an
//! HTTP contract, and a client that compiles against the server's structs stops
//! being able to detect the day the server changed them.

use serde::Deserialize;

/// One turn, in the shape `recalld::reads::TranscriptOut` serialises.
///
/// ⚠ `provenance` and `superseded_by` are NOT here because the route does not
/// send them — `tier` is the summary it sends instead. `recall show` printed
/// both from the local database; see [`crate::render::details`] for what is said
/// in their place rather than guessed.
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

/// One recorder, as `/api/sources` reports it.
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
/// ⚠ The pause is Pippijn's control. Nothing in this crate writes it; this type
/// exists so a command can REPORT the state before doing anything that assumes
/// it, which is the rule the rest of the system follows.
// Four bools, because the route sends four: `running` is what the recorders
// are doing, `desired_*` is what they were asked to do, and `settled` says
// whether those agree. Collapsing them into an enum here would be this client
// deciding what a combination means, which is the server's call.
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
/// ⚠ **A CLI needs a CREDENTIAL here, not an exemption.** `/api/*` is the gate
/// standing between the household's transcripts and anyone on the LAN
/// (`recalld::webauth`), and the tempting fix — adding these routes to
/// `DEVICE_EXEMPT`, which already holds `/api/sources` and `/api/capture` —
/// would open the archive itself to every device token on the network. The
/// Python this replaces needed no credential because it read the file directly;
/// that is not a property worth preserving.
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
    /// The `recall_session` cookie, when there is one. `None` still works for
    /// the device-exempt routes (`sources`, `capture`), which is why a missing
    /// session is not an error until something is actually refused.
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
    /// ⚠ Whitespace is trimmed because the overwhelmingly likely way this file
    /// gets written is a shell redirect that leaves a newline, and a cookie with
    /// a trailing newline is rejected as a bad signature — which reads as "my
    /// login does not work" rather than "my file has a newline in it".
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

    /// Turn a transport failure into one that says what to DO about it.
    ///
    /// A 401 here means exactly one thing — no session, or an expired one — and
    /// the bare status is the least useful way to say so.
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

    /// Replace a turn's text with a person's own words.
    ///
    /// ⚠ **The one write in this crate**, and it reaches the corrections corpus
    /// — the only thing in the archive that is not re-derivable from audio. The
    /// caller is responsible for having meant it; `recall-cli correct` requires
    /// `--apply` and prints the diff first, exactly as the Python did.
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

/// Percent-encode a query-string VALUE.
///
/// ⚠ Hand-written rather than pulled in: the alternative was a dependency for
/// one function, and the rule is narrow enough to state — everything that is not
/// an RFC 3986 unreserved character is escaped. Erring towards escaping too much
/// is safe here; erring the other way puts a household name's apostrophe into a
/// URL unquoted.
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
