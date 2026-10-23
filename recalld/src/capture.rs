//! Capture control: each person's recording window.
//!
//! Off unless a person opened a window (`record until`), which they can stop,
//! or pause and unpause up to its end. Isis holds the intent; the Mac applies
//! the household's and reports back, and a person's own phone applies theirs.
//! The API serves both: `running`/`until`/`held` as applied, `desired*` as
//! intended, and `settled` when they agree. A stored intent that does not read
//! is a fault, and a fault records: see [`Window`].

use crate::settings;
use audiocore::capture::{CaptureState, Window};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

crate::statements! {
    HOLDS_HOUSE_MIC: Meaning =
        "SELECT EXISTS (SELECT 1 FROM main.sources WHERE owner = ?1 AND kind = 'coreaudio')";
    RECORD_EVENT: Meaning =
        "INSERT INTO capture_events (utc, kind, source_id, detail) VALUES (?1, ?2, NULL, ?3)";
}

/// A [`Window`], spelled; never set means off.
const INTENT_KEY: &str = "capture_intent";
/// What the Mac last reported it had actually applied.
const REPORTED_RUNNING_KEY: &str = "capture_reported_running";
/// The Mac's gate file, spelled; blank when it did not read (a fault).
const REPORTED_WINDOW_KEY: &str = "capture_reported_window";
const REPORTED_AT_KEY: &str = "capture_reported_at";
/// Each source's last proved recording time, as the Mac reported it: a JSON
/// object of source id to ISO instant. The fleet has no liveness of its own.
const REPORTED_LIVENESS_KEY: &str = "capture_reported_source_liveness";

/// Past this the Mac's report is stale: callers show intent with
/// `micReachable: false`.
fn report_fresh() -> Duration {
    Duration::seconds(30)
}

/// The state with its `stateToken`: the fingerprint a long-poll echoes back
/// as `?known=`, the other fields' hash. It has to be the same for the same
/// state, or every long-poll returns at once.
#[must_use]
pub fn stamped(mut state: CaptureState) -> CaptureState {
    let json = serde_json::json!({
        "desiredHeld": state.desired_held,
        "desiredRunning": state.desired_running,
        "desiredUntil": state.desired_until,
        "held": state.held,
        "micReachable": state.mic_reachable,
        "running": state.running,
        "settled": state.settled,
        "until": state.until,
    })
    .to_string();
    // sha2 0.11's digest is an array with no `LowerHex`; `hex` spells it out.
    let digest = Sha256::digest(json.as_bytes());
    state.state_token = hex::encode(digest).chars().take(12).collect();
    state
}

/// The state's fields for one window at `now`: `None` is a fault, which
/// records with no end.
fn fields(window: Option<Window>, now: DateTime<Utc>) -> (bool, Option<String>, bool) {
    let window = window.map(|w| w.at(now));
    (
        window.is_none_or(|w| w.records(now)),
        window
            .and_then(Window::until)
            .map(|t| t.to_rfc3339_opts(SecondsFormat::Secs, true)),
        matches!(window, Some(Window::Hold(_))),
    )
}

/// The state from what is applied and what is wanted.
fn state(
    applied: Option<Window>,
    desired: Option<Window>,
    settled: bool,
    mic_reachable: bool,
    now: DateTime<Utc>,
) -> CaptureState {
    let (running, until, held) = fields(applied, now);
    let (desired_running, desired_until, desired_held) = fields(desired, now);
    stamped(CaptureState {
        running,
        until,
        held,
        desired_running,
        desired_until,
        desired_held,
        settled,
        mic_reachable,
        state_token: String::new(),
    })
}

/// What the Mac last said it had applied: its gate's window, `None` when that
/// did not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reported {
    pub running: bool,
    pub window: Option<Window>,
}

/// Whose window a request controls: the household's, which the Mac applies
/// and confirms, or one person's, which their own phone applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Whose {
    House,
    Person(String),
}

impl Whose {
    /// The household's for a request nobody is known on and for whoever owns
    /// the Mac's own mic (a `coreaudio` source); anyone else's is their own.
    ///
    /// # Errors
    /// If the database refuses.
    pub fn of(
        conn: &Connection,
        viewer: Option<&crate::webauth::Viewer>,
    ) -> rusqlite::Result<Self> {
        let Some(viewer) = viewer else {
            return Ok(Self::House);
        };
        let house: bool = HOLDS_HOUSE_MIC.query_row(conn, [&viewer.user_id], |r| r.get(0))?;
        Ok(if house {
            Self::House
        } else {
            Self::Person(viewer.user_id.clone())
        })
    }

    fn intent_key(&self) -> String {
        match self {
            Self::House => INTENT_KEY.to_owned(),
            Self::Person(user) => format!("{INTENT_KEY}/{user}"),
        }
    }
}

/// `whose` intent as stored: off when never set. The Mac mirrors this text.
///
/// # Errors
/// If the database refuses.
pub fn intent_text(conn: &Connection, whose: &Whose) -> rusqlite::Result<String> {
    Ok(settings::get(conn, &whose.intent_key())?.unwrap_or_else(|| Window::Off.spell()))
}

/// `whose` window at `now`; `None` when the stored intent does not read.
///
/// # Errors
/// If the database refuses.
pub fn intent_of(
    conn: &Connection,
    whose: &Whose,
    now: DateTime<Utc>,
) -> rusqlite::Result<Option<Window>> {
    Ok(Window::parse(&intent_text(conn, whose)?).map(|w| w.at(now)))
}

/// Whether the Mac reported within [`report_fresh`] of `now`.
fn report_is_fresh(conn: &Connection, now: DateTime<Utc>) -> rusqlite::Result<bool> {
    Ok(settings::get(conn, REPORTED_AT_KEY)?
        .as_deref()
        .and_then(audiocore::instant::parse)
        .is_some_and(|at| now - at.with_timezone(&Utc) <= report_fresh()))
}

/// The Mac's last report if fresh.
///
/// # Errors
/// If the database refuses.
pub fn reported_state(conn: &Connection, now: DateTime<Utc>) -> rusqlite::Result<Option<Reported>> {
    let Some(running) = settings::get(conn, REPORTED_RUNNING_KEY)? else {
        return Ok(None);
    };
    if !report_is_fresh(conn, now)? {
        return Ok(None);
    }
    Ok(Some(Reported {
        running: running == "1",
        window: settings::get(conn, REPORTED_WINDOW_KEY)?
            .as_deref()
            .and_then(Window::parse),
    }))
}

/// Record what the Mac says it has applied: `window` is its gate file
/// spelled, `None` when that did not read.
///
/// # Errors
/// If the database refuses.
pub fn record_reported(
    conn: &Connection,
    now: DateTime<Utc>,
    running: bool,
    window: Option<&str>,
    source_liveness: &BTreeMap<String, String>,
) -> rusqlite::Result<()> {
    settings::set(conn, REPORTED_RUNNING_KEY, if running { "1" } else { "0" })?;
    settings::set(conn, REPORTED_WINDOW_KEY, window.unwrap_or(""))?;
    settings::set(
        conn,
        REPORTED_AT_KEY,
        &audiocore::instant::python_isoformat_utc(now),
    )?;
    settings::set(
        conn,
        REPORTED_LIVENESS_KEY,
        &serde_json::to_string(source_liveness).expect("a string map serialises"),
    )
}
/// Each source's last proved recording time, or `None` when the Mac's report
/// is stale (an empty map: no live sources). Malformed entries are dropped.
pub fn reported_source_liveness(
    conn: &Connection,
    now: DateTime<Utc>,
) -> rusqlite::Result<Option<std::collections::HashMap<String, DateTime<Utc>>>> {
    if !report_is_fresh(conn, now)? {
        return Ok(None);
    }
    let Some(raw) = settings::get(conn, REPORTED_LIVENESS_KEY)?.filter(|s| !s.is_empty()) else {
        return Ok(Some(std::collections::HashMap::new()));
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&raw)
    else {
        return Ok(Some(std::collections::HashMap::new()));
    };
    let mut out = std::collections::HashMap::new();
    for (source, value) in parsed {
        if let Some(when) = value.as_str().and_then(audiocore::instant::parse) {
            out.insert(source, when.with_timezone(&Utc));
        }
    }
    Ok(Some(out))
}

/// `whose` capture state: the household's is [`fleet_capture_state`]; a
/// person's phone applies their intent itself, with no Mac to confirm it, so
/// it reads as applied and settled.
///
/// # Errors
/// If the database refuses.
pub fn capture_state_of(
    conn: &Connection,
    whose: &Whose,
    now: DateTime<Utc>,
) -> rusqlite::Result<CaptureState> {
    if *whose == Whose::House {
        return fleet_capture_state(conn, now);
    }
    let desired = intent_of(conn, whose, now)?;
    Ok(state(desired, desired, true, true, now))
}

/// The fleet's view: intent, plus the Mac's confirmation of it.
///
/// # Errors
/// If the database refuses.
pub fn fleet_capture_state(
    conn: &Connection,
    now: DateTime<Utc>,
) -> rusqlite::Result<CaptureState> {
    let desired = intent_of(conn, &Whose::House, now)?;
    let Some(reported) = reported_state(conn, now)? else {
        return Ok(state(desired, desired, false, false, now));
    };
    // The whole window must match, so moving an end reads as transitioning
    // until applied.
    let applied = reported.window.map(|w| w.at(now));
    Ok(state(applied, desired, applied == desired, true, now))
}

/// Set `whose` window and wake every poll.
///
/// # Errors
/// If the database refuses.
pub fn set_intent(conn: &Connection, whose: &Whose, window: Window) -> rusqlite::Result<()> {
    settings::set(conn, &whose.intent_key(), &window.spell())?;
    notify_intent_changed();
    Ok(())
}

/// What a press asks of `whose` window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Press {
    /// Record until this time, at most a day ahead.
    Record(DateTime<Utc>),
    Stop,
    Pause,
    Unpause,
}

impl Press {
    const fn verb(self) -> &'static str {
        match self {
            Self::Record(_) => "record",
            Self::Stop => "stop",
            Self::Pause => "pause",
            Self::Unpause => "unpause",
        }
    }
}

/// Apply `press` to `whose` window and return the window now. A fault reads as
/// off here: whoever presses pause wants quiet, not recording.
///
/// # Errors
/// If the database refuses.
pub fn press(
    conn: &Connection,
    whose: &Whose,
    now: DateTime<Utc>,
    press: Press,
) -> rusqlite::Result<Window> {
    let current = intent_of(conn, whose, now)?.unwrap_or(Window::Off);
    let next = match press {
        Press::Record(until) => Window::open(now, until),
        Press::Stop => Window::Off,
        Press::Pause => current.paused(),
        Press::Unpause => current.unpaused(),
    };
    set_intent(conn, whose, next)?;
    Ok(next)
}

/// Record who pressed what. Capture control needs no login, so
/// this carries the request's origin.
pub fn record_control_origin(
    conn: &Connection,
    now: DateTime<Utc>,
    verb: &str,
    origin: &str,
) -> rusqlite::Result<()> {
    RECORD_EVENT.execute(
        conn,
        rusqlite::params![
            audiocore::instant::python_isoformat_utc(now),
            "control_request",
            format!("{verb} — {origin}"),
        ],
    )?;
    Ok(())
}

// --- the HTTP surface -------------------------------------------------------

use crate::route;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use std::sync::Arc;

/// The capture routes' state: the gate config, to say who pressed the button.
pub struct Control {
    pub root: std::path::PathBuf,
    pub webauth: Arc<crate::webauth::Config>,
    pub trusted_proxies: Vec<std::net::IpAddr>,
}

/// Who connected: the peer, or the `X-Real-IP` a trusted proxy sends. isis's
/// front door connects from the node for every caller; anyone else's header is
/// ignored.
#[must_use]
pub fn client_host(
    peer: Option<std::net::IpAddr>,
    real_ip: Option<&str>,
    trusted: &[std::net::IpAddr],
) -> Option<String> {
    let peer = peer?;
    let forwarded = real_ip.and_then(|h| h.trim().parse::<std::net::IpAddr>().ok());
    match forwarded {
        Some(client) if trusted.contains(&peer) => Some(client.to_string()),
        _ => Some(peer.to_string()),
    }
}

/// "Capture intent changed". A `watch` rather than a `Notify`: a receiver
/// remembers the version it saw, so a press between deriving the state and
/// waiting is not lost.
static INTENT_CHANGED: std::sync::LazyLock<tokio::sync::watch::Sender<u64>> =
    std::sync::LazyLock::new(|| tokio::sync::watch::channel(0).0);

/// Subscribe before deriving state.
#[must_use]
pub fn intent_watch() -> tokio::sync::watch::Receiver<u64> {
    INTENT_CHANGED.subscribe()
}

pub fn notify_intent_changed() {
    INTENT_CHANGED.send_modify(|v| *v = v.wrapping_add(1));
}

/// Park until the intent changes (`true`) or `slice` elapses. Callers must
/// still re-derive on the slice: a pause elapsing, and a pause written by
/// another process, signal nothing.
pub async fn wait_intent_changed(
    mut watcher: tokio::sync::watch::Receiver<u64>,
    slice: std::time::Duration,
) -> bool {
    tokio::time::timeout(slice, watcher.changed()).await.is_ok()
}

/// The longest a request is held.
pub const WAIT_CAP: std::time::Duration = std::time::Duration::from_secs(25);
/// How often a held request re-derives the state, for changes nothing signals.
pub const WAIT_SLICE: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Deserialize)]
pub struct StatusQuery {
    /// Seconds to long-poll; 0 answers at once.
    #[serde(default)]
    pub wait: f64,
    /// The `stateToken` the caller last saw; the request hangs while it holds.
    #[serde(default)]
    pub known: String,
}

/// `GET /api/capture`. The long-poll makes a press propagate in about a round
/// trip, and paces the Mac's mirror.
pub async fn status_route(
    State(st): State<Arc<Control>>,
    asked: crate::webauth::Asked,
    Query(q): Query<StatusQuery>,
) -> Response {
    let viewer = crate::webauth::viewer(asked);
    let root = st.root.clone();
    let wait = q.wait.clamp(0.0, WAIT_CAP.as_secs_f64());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(wait);

    loop {
        let watcher = intent_watch();
        let root = root.clone();
        let viewer = viewer.clone();
        let state = match route::blocking("capture", move || {
            let conn = crate::work::open_write(&root)?;
            let whose = Whose::of(&conn, viewer.as_ref())?;
            capture_state_of(&conn, &whose, chrono::Utc::now())
        })
        .await
        {
            Ok(state) => state,
            Err(response) => return response,
        };
        if state.state_token != q.known || std::time::Instant::now() >= deadline {
            return axum::Json(state).into_response();
        }
        wait_intent_changed(
            watcher,
            WAIT_SLICE.min(deadline - std::time::Instant::now()),
        )
        .await;
    }
}

#[derive(Deserialize)]
pub struct RecordQuery {
    /// When to stop, an instant; a day ahead at most.
    pub until: String,
}

/// The request fields the audit needs.
struct Asker {
    method: String,
    path: String,
    cookie: Option<String>,
    authorization: Option<String>,
    host: Option<String>,
}

impl Asker {
    fn from(parts: &axum::http::request::Parts, host: Option<String>) -> Self {
        let header = |name: &str| {
            parts
                .headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(ToOwned::to_owned)
        };
        Self {
            method: parts.method.as_str().to_owned(),
            path: parts.uri.path().to_owned(),
            cookie: header("cookie"),
            authorization: header("authorization"),
            host,
        }
    }

    fn describe(&self, cfg: &crate::webauth::Config, now: i64) -> String {
        crate::webauth::request_origin(
            cfg,
            &self.method,
            &self.path,
            self.cookie.as_deref(),
            self.authorization.as_deref(),
            now,
            self.host.as_deref(),
        )
    }
}

/// Record who asked. A failure is logged, never fatal: the action has happened.
fn audit(conn: &Connection, verb: &str, origin: &str) {
    if let Err(err) = record_control_origin(conn, chrono::Utc::now(), verb, origin) {
        tracing::warn!("could not record capture-control origin ({verb}): {err}");
    }
}

/// `POST /api/capture/record?until=`: open a window, or move its end.
/// Answers unsettled until the Mac applies it.
pub async fn record_route(
    State(st): State<Arc<Control>>,
    Query(q): Query<RecordQuery>,
    request: axum::extract::Request,
) -> Response {
    let Some(until) = audiocore::instant::parse_utc(&q.until) else {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            "until is not an instant",
        )
            .into_response();
    };
    control(st, Press::Record(until), request).await
}

/// `POST /api/capture/stop`.
pub async fn stop_route(
    State(st): State<Arc<Control>>,
    request: axum::extract::Request,
) -> Response {
    control(st, Press::Stop, request).await
}

/// `POST /api/capture/pause`: stop recording, keeping the window's end.
pub async fn pause_route(
    State(st): State<Arc<Control>>,
    request: axum::extract::Request,
) -> Response {
    control(st, Press::Pause, request).await
}

/// `POST /api/capture/unpause`: record again, to the window's end.
pub async fn unpause_route(
    State(st): State<Arc<Control>>,
    request: axum::extract::Request,
) -> Response {
    control(st, Press::Unpause, request).await
}

async fn control(st: Arc<Control>, pressed: Press, request: axum::extract::Request) -> Response {
    let (parts, _) = request.into_parts();
    let peer = parts
        .extensions
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|info| info.0.ip());
    let real_ip = parts.headers.get("x-real-ip").and_then(|v| v.to_str().ok());
    let host = client_host(peer, real_ip, &st.trusted_proxies);
    let asker = Asker::from(&parts, host);
    let viewer = parts.extensions.get::<crate::webauth::Viewer>().cloned();
    let cfg = st.webauth.clone();
    let root = st.root.clone();

    route::json("capture-control", move || {
        let conn = crate::work::open_write(&root)?;
        let now = chrono::Utc::now();
        let whose = Whose::of(&conn, viewer.as_ref())?;
        press(&conn, &whose, now, pressed)?;
        // After the action, so the audit cannot prevent it.
        audit(
            &conn,
            pressed.verb(),
            &asker.describe(&cfg, now.timestamp()),
        );
        capture_state_of(&conn, &whose, now)
    })
    .await
}
