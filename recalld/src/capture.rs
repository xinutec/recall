//! The capture-control state, as the fleet holds it.
//!
//! Isis is the system of record for capture intent but runs no capture agent: the
//! Mac actuates and reports back what it applied. The two can disagree for a
//! couple of mirror cycles, so the API serves both, `running`/`pausedUntil` as
//! the microphone's confirmed word and `desired*` as the intent, with `settled`
//! saying whether they agree. A client renders the disagreement as "Pausing…"
//! rather than flapping between two truths. This is the household's privacy
//! control: a pause nobody can confirm took effect is worthless, which is why the
//! two halves are separate fields.

use chrono::{DateTime, Duration, Utc};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};

/// ISO resume-by; blank or absent means running.
const INTENT_KEY: &str = "capture_intent";
/// What the Mac last reported it had actually applied.
const REPORTED_RUNNING_KEY: &str = "capture_reported_running";
const REPORTED_PAUSED_KEY: &str = "capture_reported_paused_until";
const REPORTED_AT_KEY: &str = "capture_reported_at";
/// Each source's last-proved-recording time, as the Mac last reported it — a
/// JSON object of `source_id` to ISO instant. The fleet has no liveness markers
/// of its own, so this is the only thing `/api/sources` can say about a mic.
const REPORTED_LIVENESS_KEY: &str = "capture_reported_source_liveness";

/// How recent the Mac's report must be to be believed. Past this the Mac has
/// stopped reporting and the caller shows intent instead, with
/// `micReachable: false` saying so.
fn report_fresh() -> Duration {
    Duration::seconds(30)
}

/// The wire shape of `GET /api/capture`. Field order and spelling are a
/// contract: `stateToken` is a hash over this object, so a renamed or reordered
/// field changes every client's long-poll. The bools are the wire shape, so an
/// enum would change the contract rather than tidy it.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct CaptureState {
    pub running: bool,
    #[serde(rename = "pausedUntil")]
    pub paused_until: Option<String>,
    #[serde(rename = "desiredRunning")]
    pub desired_running: bool,
    #[serde(rename = "desiredPausedUntil")]
    pub desired_paused_until: Option<String>,
    pub settled: bool,
    #[serde(rename = "micReachable")]
    pub mic_reachable: bool,
    #[serde(rename = "stateToken")]
    pub state_token: String,
}

/// The fingerprint a long-poll echoes back as `?known=`, so "unchanged" is the
/// server's judgement rather than the client's comparison: `sha256` of the
/// fields except `stateToken`, keys sorted, `, ` and `: ` separators. The struct's
/// field order is the sorted key order; a wrong spelling does not fail, it turns
/// every long-poll into a busy poll.
#[allow(clippy::struct_excessive_bools)]
#[derive(Serialize)]
struct TokenPayload<'a> {
    #[serde(rename = "desiredPausedUntil")]
    desired_paused_until: &'a Option<String>,
    #[serde(rename = "desiredRunning")]
    desired_running: bool,
    #[serde(rename = "micReachable")]
    mic_reachable: bool,
    #[serde(rename = "pausedUntil")]
    paused_until: &'a Option<String>,
    running: bool,
    settled: bool,
}

impl CaptureState {
    /// Stamp `stateToken` from the rest of the fields.
    #[must_use]
    pub fn stamped(mut self) -> Self {
        self.state_token = self.token();
        self
    }

    fn token(&self) -> String {
        // The struct's field order IS the sorted key order Python emits; a
        // field added out of alphabetical order here would break the hash, so
        // the test below pins a live-server value rather than trusting this.
        let payload = TokenPayload {
            desired_paused_until: &self.desired_paused_until,
            desired_running: self.desired_running,
            mic_reachable: self.mic_reachable,
            paused_until: &self.paused_until,
            running: self.running,
            settled: self.settled,
        };
        let json = crate::pyjson::dump(&payload);
        // sha2 0.11's digest is an array with no `LowerHex`; `hex` spells it out.
        let digest = Sha256::digest(json.as_bytes());
        hex::encode(digest).chars().take(12).collect()
    }
}

/// What the Mac last said it had applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reported {
    pub running: bool,
    pub paused_until: Option<String>,
}

fn setting(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
        row.get::<_, String>(0)
    })
    .optional()
}

/// The desired resume-by, or `None` when running. An elapsed intent reads as
/// running: a pause that outlived its own deadline must never keep a household
/// silent because nobody cleared a row. Returns the stored spelling, because the
/// Mac echoes this exact string back and `settled` compares the two by equality.
pub fn intent_until(conn: &Connection, now: DateTime<Utc>) -> rusqlite::Result<Option<String>> {
    let Some(raw) = setting(conn, INTENT_KEY)? else {
        return Ok(None);
    };
    if raw.is_empty() {
        return Ok(None);
    }
    let Some(parsed) = audiocore::instant::parse(&raw) else {
        // Unparseable reads as RUNNING, never as a pause nobody can clear.
        return Ok(None);
    };
    if parsed.with_timezone(&Utc) <= now {
        return Ok(None);
    }
    Ok(audiocore::instant::respell_utc(&raw))
}

/// The Mac's last-reported state if it is fresh, else `None` — meaning the Mac
/// has stopped reporting, so the caller shows intent and says the mic is not
/// reachable.
pub fn reported_state(conn: &Connection, now: DateTime<Utc>) -> rusqlite::Result<Option<Reported>> {
    let (Some(at), Some(running)) = (
        setting(conn, REPORTED_AT_KEY)?,
        setting(conn, REPORTED_RUNNING_KEY)?,
    ) else {
        return Ok(None);
    };
    if at.is_empty() {
        return Ok(None);
    }
    let Some(at) = audiocore::instant::parse(&at) else {
        return Ok(None);
    };
    let at = at.with_timezone(&Utc);
    if now - at > report_fresh() {
        return Ok(None);
    }
    Ok(Some(Reported {
        running: running == "1",
        paused_until: setting(conn, REPORTED_PAUSED_KEY)?.filter(|s| !s.is_empty()),
    }))
}

/// Record what the Mac says it currently has applied. `paused_until` is stored
/// verbatim: [`fleet_capture_state`] decides `settled` by comparing it to the
/// intent as strings. `REPORTED_AT_KEY` is what makes the rest believable; every
/// reader gates on its freshness.
pub fn record_reported(
    conn: &Connection,
    now: DateTime<Utc>,
    running: bool,
    paused_until: Option<&str>,
    source_liveness: &serde_json::Map<String, serde_json::Value>,
) -> rusqlite::Result<()> {
    set_setting(conn, REPORTED_RUNNING_KEY, if running { "1" } else { "0" })?;
    set_setting(conn, REPORTED_PAUSED_KEY, paused_until.unwrap_or(""))?;
    set_setting(
        conn,
        REPORTED_AT_KEY,
        &audiocore::instant::python_isoformat_utc(now),
    )?;
    set_setting(
        conn,
        REPORTED_LIVENESS_KEY,
        &crate::pyjson::dump(source_liveness),
    )
}

/// Each source's last-proved-recording time as the Mac last reported it, or
/// `None` when the Mac has stopped checking in. Behind the same freshness gate
/// as [`reported_state`]: the fleet has no liveness of its own, so a stale
/// report must read as "we cannot see" rather than as the last thing heard.
/// `None` is a quiet Mac; an empty map is a Mac reporting no live sources. A
/// malformed entry is dropped, not fatal.
pub fn reported_source_liveness(
    conn: &Connection,
    now: DateTime<Utc>,
) -> rusqlite::Result<Option<std::collections::HashMap<String, DateTime<Utc>>>> {
    let Some(at) = setting(conn, REPORTED_AT_KEY)?.filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let Some(at) = audiocore::instant::parse(&at) else {
        return Ok(None);
    };
    if now - at.with_timezone(&Utc) > report_fresh() {
        return Ok(None);
    }
    let Some(raw) = setting(conn, REPORTED_LIVENESS_KEY)?.filter(|s| !s.is_empty()) else {
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

/// The fleet's view: intent, plus the Mac's confirmation of it.
pub fn fleet_capture_state(
    conn: &Connection,
    now: DateTime<Utc>,
) -> rusqlite::Result<CaptureState> {
    let desired_until = intent_until(conn, now)?;
    let desired_running = desired_until.is_none();

    let Some(reported) = reported_state(conn, now)? else {
        // Nothing fresh from the Mac. Show the intent and say plainly that the
        // mic is not reachable, rather than presenting intent as confirmation.
        return Ok(CaptureState {
            running: desired_running,
            paused_until: desired_until.clone(),
            desired_running,
            desired_paused_until: desired_until,
            settled: false,
            mic_reachable: false,
            state_token: String::new(),
        }
        .stamped());
    };

    // Settled = the mic confirmed the desired state. When paused the resume-by
    // must match too, so EXTENDING a pause (a snooze) reads as transitioning
    // until applied. The Mac round-trips the intent's exact ISO string, so this
    // equality is exact rather than a tolerance.
    let settled = reported.running == desired_running
        && (desired_running || reported.paused_until == desired_until);

    Ok(CaptureState {
        running: reported.running,
        paused_until: reported.paused_until,
        desired_running,
        desired_paused_until: desired_until,
        settled,
        mic_reachable: true,
        state_token: String::new(),
    }
    .stamped())
}

/// A pause is bounded, always: "stop recording", never "stop indefinitely". A
/// pause that outlives everyone's memory of setting it is how a week of the
/// archive goes missing without anyone deciding to lose it.
fn max_pause() -> Duration {
    Duration::hours(24)
}

/// When a pause starting at `now` must end, clamped to `max_pause`.
///
/// A negative or absent `minutes` is not an error: `None` means "the full
/// bound", and a negative one clamps to zero rather than minting a pause that
/// has already elapsed.
pub fn compute_resume_by(now: DateTime<Utc>, minutes: Option<i64>) -> DateTime<Utc> {
    let span = match minutes {
        None => max_pause(),
        Some(m) => Duration::minutes(m).min(max_pause()),
    };
    now + span.max(Duration::zero())
}

fn set_setting(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [key, value],
    )?;
    Ok(())
}

/// Record a bounded pause as the fleet's desired state and return its resume-by
/// in the spelling that is stored and compared. Intent, not actuation: the Mac's
/// mirror applies it and reports back.
pub fn intent_pause(
    conn: &Connection,
    now: DateTime<Utc>,
    minutes: Option<i64>,
) -> rusqlite::Result<String> {
    let until = compute_resume_by(now, minutes);
    let iso = audiocore::instant::python_isoformat_utc(until);
    set_setting(conn, INTENT_KEY, &iso)?;
    notify_intent_changed();
    Ok(iso)
}

/// Record "run" as the desired state, as an empty value rather than a deleted
/// row, so a reader never has to tell "never paused" from "resumed".
pub fn intent_resume(conn: &Connection) -> rusqlite::Result<()> {
    set_setting(conn, INTENT_KEY, "")?;
    notify_intent_changed();
    Ok(())
}

/// Append the audit record of who asked for a pause or resume. Capture control
/// is login-free, so the agent's own event cannot name a caller; this carries
/// the request's origin. Best-effort by contract: a failed audit must never fail
/// the control action.
pub fn record_control_origin(
    conn: &Connection,
    now: DateTime<Utc>,
    verb: &str,
    origin: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO capture_events (utc, kind, source_id, detail) VALUES (?1, ?2, NULL, ?3)",
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

/// What the capture routes need, distinct from [`CaptureState`], which is what
/// they serve. Its own type because capture is the one route family that needs
/// the gate config: to say who pressed the button on a plane that requires no
/// login.
pub struct Control {
    pub root: std::path::PathBuf,
    pub webauth: Option<Arc<crate::webauth::Config>>,
}

/// The process-global "capture intent changed" signal. A `watch` channel rather
/// than a `Notify`: a receiver remembers the version it last saw, so a press
/// landing between deriving the state and starting the wait is not lost.
/// Subscribe before the derive.
static INTENT_CHANGED: std::sync::LazyLock<tokio::sync::watch::Sender<u64>> =
    std::sync::LazyLock::new(|| tokio::sync::watch::channel(0).0);

/// Subscribe before deriving state; hand the result to [`wait_intent_changed`].
#[must_use]
pub fn intent_watch() -> tokio::sync::watch::Receiver<u64> {
    INTENT_CHANGED.subscribe()
}

/// Announce that the capture intent moved. Called by every in-process writer.
pub fn notify_intent_changed() {
    INTENT_CHANGED.send_modify(|v| *v = v.wrapping_add(1));
}

/// Park until the intent changes or `slice` elapses. `true` means a change. The
/// timeout is the correctness floor, not a fallback: a pause elapsing has no
/// writer, and a break-glass pause writes from another process, so the caller
/// must still re-derive on the slice.
pub async fn wait_intent_changed(
    mut watcher: tokio::sync::watch::Receiver<u64>,
    slice: std::time::Duration,
) -> bool {
    tokio::time::timeout(slice, watcher.changed()).await.is_ok()
}

/// Never hold a request past this — proxies and thread pools need a horizon.
const WAIT_CAP: std::time::Duration = std::time::Duration::from_secs(25);
/// Re-derive the state this often while hanging. Transitions with NO notify —
/// a pause elapsing, a report ageing out of freshness, a break-glass CLI pause
/// writing the file directly — surface within one slice rather than never.
const WAIT_SLICE: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Deserialize)]
pub struct StatusQuery {
    /// Seconds to long-poll. Absent or 0 answers at once, which is what an
    /// older client that does not know about hanging expects.
    #[serde(default)]
    pub wait: f64,
    /// The `stateToken` the caller last saw. The request hangs while the state
    /// still fingerprints to this.
    #[serde(default)]
    pub known: String,
}

/// `GET /api/capture`: is the household being recorded, and if paused, until
/// when. The long-poll makes a press propagate in about a round trip, and it
/// paces the Mac's mirror, which hangs here while its intent is unchanged.
pub async fn status_route(
    State(st): State<Arc<Control>>,
    Query(q): Query<StatusQuery>,
) -> Response {
    let root = st.root.clone();
    let wait = q.wait.clamp(0.0, WAIT_CAP.as_secs_f64());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(wait);

    loop {
        // Subscribe before deriving, so a press landing between the read and the wait
        // returns from the wait at once.
        let watcher = intent_watch();
        let root = root.clone();
        let state = match route::blocking("capture", move || {
            fleet_capture_state(&crate::work::open_write(&root)?, chrono::Utc::now())
        })
        .await
        {
            Ok(state) => state,
            Err(response) => return response,
        };
        if state.state_token != q.known || std::time::Instant::now() >= deadline {
            return axum::Json(state).into_response();
        }
        // Notify for the fast path, the slice as the floor: a pause elapsing and a
        // break-glass pause have no writer that could signal.
        wait_intent_changed(
            watcher,
            WAIT_SLICE.min(deadline - std::time::Instant::now()),
        )
        .await;
    }
}

/// How long a pause lasts when the caller does not say. The UI always sends a
/// number; this is the bound for anything that does not.
#[derive(Deserialize)]
pub struct PauseQuery {
    pub minutes: Option<i64>,
}

/// The request fields the audit needs, lifted out of the extractors so the
/// handlers stay readable and the descriptor stays testable.
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

    fn describe(&self, cfg: Option<&crate::webauth::Config>, now: i64) -> String {
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

/// Record who asked, without ever being able to refuse the action: a failure is
/// logged and swallowed, the control action has already happened.
fn audit(conn: &Connection, verb: &str, origin: &str) {
    if let Err(err) = record_control_origin(conn, chrono::Utc::now(), verb, origin) {
        tracing::warn!("could not record capture-control origin ({verb}): {err}");
    }
}

/// `POST /api/capture/pause`. Records intent; the Mac's mirror applies it and
/// reports back, so the answer comes back unsettled, which is the truth rather
/// than a delay.
pub async fn pause_route(
    State(st): State<Arc<Control>>,
    Query(q): Query<PauseQuery>,
    request: axum::extract::Request,
) -> Response {
    control(st, Intent::Pause { minutes: q.minutes }, request).await
}

/// `POST /api/capture/resume` — start capture again now.
pub async fn resume_route(
    State(st): State<Arc<Control>>,
    request: axum::extract::Request,
) -> Response {
    control(st, Intent::Resume, request).await
}

/// What the caller asked for. An enum rather than nested options because the
/// three cases read as three different things: resume, pause for the full
/// bound, pause for a stated number of minutes.
enum Intent {
    Resume,
    Pause { minutes: Option<i64> },
}

async fn control(st: Arc<Control>, intent: Intent, request: axum::extract::Request) -> Response {
    let (parts, _) = request.into_parts();
    let host = parts
        .extensions
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|info| info.0.ip().to_string());
    let asker = Asker::from(&parts, host);
    let cfg = st.webauth.clone();
    let root = st.root.clone();

    route::json("capture-control", move || {
        let conn = crate::work::open_write(&root)?;
        let now = chrono::Utc::now();
        let verb = match intent {
            Intent::Pause { minutes } => {
                intent_pause(&conn, now, minutes)?;
                "pause"
            }
            Intent::Resume => {
                intent_resume(&conn)?;
                "resume"
            }
        };
        // AFTER the action, never before: the audit annotates a decision that
        // has already been taken, and must not be able to prevent it.
        audit(
            &conn,
            verb,
            &asker.describe(cfg.as_deref(), now.timestamp()),
        );
        fleet_capture_state(&conn, now)
    })
    .await
}
