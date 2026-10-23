//! Capture control: the household's pause.
//!
//! Isis holds the intent; the Mac applies it and reports back. The API serves
//! both: `running`/`pausedUntil` as the Mac confirmed them, `desired*` as
//! intended, and `settled` when they agree. A pause nobody can confirm is
//! worthless, hence the two halves.

use crate::settings;
use audiocore::capture::CaptureState;
use chrono::{DateTime, Duration, Utc};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

crate::statements! {
    HOLDS_HOUSE_MIC: Meaning =
        "SELECT EXISTS (SELECT 1 FROM main.sources WHERE owner = ?1 AND kind = 'coreaudio')";
    RECORD_EVENT: Meaning =
        "INSERT INTO capture_events (utc, kind, source_id, detail) VALUES (?1, ?2, NULL, ?3)";
}

/// ISO resume-by; blank or absent means running.
const INTENT_KEY: &str = "capture_intent";
/// What the Mac last reported it had actually applied.
const REPORTED_RUNNING_KEY: &str = "capture_reported_running";
const REPORTED_PAUSED_KEY: &str = "capture_reported_paused_until";
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
        "desiredPausedUntil": state.desired_paused_until,
        "desiredRunning": state.desired_running,
        "micReachable": state.mic_reachable,
        "pausedUntil": state.paused_until,
        "running": state.running,
        "settled": state.settled,
    })
    .to_string();
    // sha2 0.11's digest is an array with no `LowerHex`; `hex` spells it out.
    let digest = Sha256::digest(json.as_bytes());
    state.state_token = hex::encode(digest).chars().take(12).collect();
    state
}

/// What the Mac last said it had applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reported {
    pub running: bool,
    pub paused_until: Option<String>,
}

/// Whose pause a request controls: the household's, which the Mac applies and
/// confirms, or one person's, which their own phone applies.
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

/// The desired resume-by, or `None` when running. An elapsed or unreadable
/// intent reads as running. Returns the stored spelling: the Mac echoes it and
/// `settled` compares strings.
pub fn intent_until(conn: &Connection, now: DateTime<Utc>) -> rusqlite::Result<Option<String>> {
    intent_until_of(conn, &Whose::House, now)
}

/// [`intent_until`] for `whose` pause.
///
/// # Errors
/// If the database refuses.
pub fn intent_until_of(
    conn: &Connection,
    whose: &Whose,
    now: DateTime<Utc>,
) -> rusqlite::Result<Option<String>> {
    let Some(raw) = settings::get(conn, &whose.intent_key())? else {
        return Ok(None);
    };
    if raw.is_empty() {
        return Ok(None);
    }
    let Some(parsed) = audiocore::instant::parse(&raw) else {
        return Ok(None);
    };
    if parsed.with_timezone(&Utc) <= now {
        return Ok(None);
    }
    Ok(audiocore::instant::respell_utc(&raw))
}

/// Whether the Mac reported within [`report_fresh`] of `now`.
fn report_is_fresh(conn: &Connection, now: DateTime<Utc>) -> rusqlite::Result<bool> {
    Ok(settings::get(conn, REPORTED_AT_KEY)?
        .as_deref()
        .and_then(audiocore::instant::parse)
        .is_some_and(|at| now - at.with_timezone(&Utc) <= report_fresh()))
}

/// The Mac's last report if fresh.
pub fn reported_state(conn: &Connection, now: DateTime<Utc>) -> rusqlite::Result<Option<Reported>> {
    let Some(running) = settings::get(conn, REPORTED_RUNNING_KEY)? else {
        return Ok(None);
    };
    if !report_is_fresh(conn, now)? {
        return Ok(None);
    }
    Ok(Some(Reported {
        running: running == "1",
        paused_until: settings::get(conn, REPORTED_PAUSED_KEY)?.filter(|s| !s.is_empty()),
    }))
}

/// Record what the Mac says it has applied. `paused_until` is stored verbatim
/// for the string comparison behind `settled`.
pub fn record_reported(
    conn: &Connection,
    now: DateTime<Utc>,
    running: bool,
    paused_until: Option<&str>,
    source_liveness: &BTreeMap<String, String>,
) -> rusqlite::Result<()> {
    settings::set(conn, REPORTED_RUNNING_KEY, if running { "1" } else { "0" })?;
    settings::set(conn, REPORTED_PAUSED_KEY, paused_until.unwrap_or(""))?;
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
    let desired_until = intent_until_of(conn, whose, now)?;
    let desired_running = desired_until.is_none();
    Ok(stamped(CaptureState {
        running: desired_running,
        paused_until: desired_until.clone(),
        desired_running,
        desired_paused_until: desired_until,
        settled: true,
        mic_reachable: true,
        state_token: String::new(),
    }))
}

/// The fleet's view: intent, plus the Mac's confirmation of it.
pub fn fleet_capture_state(
    conn: &Connection,
    now: DateTime<Utc>,
) -> rusqlite::Result<CaptureState> {
    let desired_until = intent_until(conn, now)?;
    let desired_running = desired_until.is_none();

    let Some(reported) = reported_state(conn, now)? else {
        return Ok(stamped(CaptureState {
            running: desired_running,
            paused_until: desired_until.clone(),
            desired_running,
            desired_paused_until: desired_until,
            settled: false,
            mic_reachable: false,
            state_token: String::new(),
        }));
    };

    // When paused the resume-by must match too, so extending a pause reads as
    // transitioning until applied.
    let settled = reported.running == desired_running
        && (desired_running || reported.paused_until == desired_until);

    Ok(stamped(CaptureState {
        running: reported.running,
        paused_until: reported.paused_until,
        desired_running,
        desired_paused_until: desired_until,
        settled,
        mic_reachable: true,
        state_token: String::new(),
    }))
}

/// A pause is always bounded: a forgotten one would silently lose days of the
/// archive.
fn max_pause() -> Duration {
    Duration::hours(24)
}

/// When a pause starting at `now` ends: `None` means the full bound, and a
/// negative count clamps to zero.
pub fn compute_resume_by(now: DateTime<Utc>, minutes: Option<i64>) -> DateTime<Utc> {
    let span = match minutes {
        None => max_pause(),
        Some(m) => Duration::minutes(m).min(max_pause()),
    };
    now + span.max(Duration::zero())
}

/// Record a bounded pause as the intent and return its resume-by, as stored.
pub fn intent_pause(
    conn: &Connection,
    now: DateTime<Utc>,
    minutes: Option<i64>,
) -> rusqlite::Result<String> {
    intent_pause_of(conn, &Whose::House, now, minutes)
}

/// [`intent_pause`] for `whose` pause.
///
/// # Errors
/// If the database refuses.
pub fn intent_pause_of(
    conn: &Connection,
    whose: &Whose,
    now: DateTime<Utc>,
    minutes: Option<i64>,
) -> rusqlite::Result<String> {
    let until = compute_resume_by(now, minutes);
    let iso = audiocore::instant::python_isoformat_utc(until);
    settings::set(conn, &whose.intent_key(), &iso)?;
    notify_intent_changed();
    Ok(iso)
}

/// Record "run" as the intent: an empty value, not a deleted row.
pub fn intent_resume(conn: &Connection) -> rusqlite::Result<()> {
    intent_resume_of(conn, &Whose::House)
}

/// [`intent_resume`] for `whose` pause.
///
/// # Errors
/// If the database refuses.
pub fn intent_resume_of(conn: &Connection, whose: &Whose) -> rusqlite::Result<()> {
    settings::set(conn, &whose.intent_key(), "")?;
    notify_intent_changed();
    Ok(())
}

/// Record who asked for a pause or resume. Capture control needs no login, so
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
pub struct PauseQuery {
    /// Absent: the full bound.
    pub minutes: Option<i64>,
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

/// `POST /api/capture/pause`. Answers unsettled until the Mac applies it.
pub async fn pause_route(
    State(st): State<Arc<Control>>,
    Query(q): Query<PauseQuery>,
    request: axum::extract::Request,
) -> Response {
    control(st, Intent::Pause { minutes: q.minutes }, request).await
}

/// `POST /api/capture/resume`.
pub async fn resume_route(
    State(st): State<Arc<Control>>,
    request: axum::extract::Request,
) -> Response {
    control(st, Intent::Resume, request).await
}

enum Intent {
    Resume,
    Pause { minutes: Option<i64> },
}

async fn control(st: Arc<Control>, intent: Intent, request: axum::extract::Request) -> Response {
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
        let verb = match intent {
            Intent::Pause { minutes } => {
                intent_pause_of(&conn, &whose, now, minutes)?;
                "pause"
            }
            Intent::Resume => {
                intent_resume_of(&conn, &whose)?;
                "resume"
            }
        };
        // After the action, so the audit cannot prevent it.
        audit(&conn, verb, &asker.describe(&cfg, now.timestamp()));
        capture_state_of(&conn, &whose, now)
    })
    .await
}
