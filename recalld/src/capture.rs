//! The capture-control state, as the fleet holds it.
//!
//! Isis is the system of record for capture INTENT but runs no capture agent:
//! the Mac actuates and reports back what it applied. So the two can disagree
//! for a couple of mirror cycles, and the API serves BOTH — `running` /
//! `pausedUntil` carry the mic's confirmed word, `desired*` carries the intent,
//! and `settled` says whether they agree. A client renders the disagreement as
//! "Pausing…"/"Resuming…" rather than flapping between two truths it cannot
//! tell apart.
//!
//! ⚠ **This is the household's privacy control.** A pause nobody can confirm
//! took effect is worthless, which is the whole reason the confirmed and the
//! desired halves are separate fields rather than one.

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

/// How recent the Mac's report must be to be believed. Past this the Mac has
/// stopped reporting and the caller shows intent instead, with
/// `micReachable: false` saying so.
fn report_fresh() -> Duration {
    Duration::seconds(30)
}

/// The wire shape of `GET /api/capture`.
///
/// ⚠ Field order and spelling are a CONTRACT, not a style choice: `stateToken`
/// is a hash over this object's JSON, so a renamed or reordered field changes
/// every client's long-poll. See [`CaptureState::token`].
// Four bools, and clippy is right that a struct of them is usually a smell.
// Here they are the WIRE SHAPE — `running`/`desiredRunning`/`settled`/
// `micReachable` are what the client renders and what the token hashes — so
// collapsing them into an enum would change the contract, not tidy it.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
/// server's judgement rather than the client's field-by-field comparison.
///
/// ⚠ **Byte-exact against the Python, deliberately.** It is
/// `sha256(json.dumps(payload, sort_keys=True))[:12]` over every field EXCEPT
/// `stateToken` — so it needs Python's separators (`", "` and `": "`), Python's
/// key order (sorted, not declaration order), and Python's `null`. Get any of
/// those wrong and the token simply never matches what a client last saw, which
/// does not fail: it silently turns every long-poll into a busy poll.
// Same reason as CaptureState: this struct's SHAPE is the hashed contract.
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
        let digest = Sha256::digest(json.as_bytes());
        format!("{digest:x}").chars().take(12).collect()
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

/// The desired resume-by, or `None` when running.
///
/// ⚠ An ELAPSED intent reads as running — the same bounded-pause safety net the
/// local pause file has. A pause that outlived its own deadline must never keep
/// a household silent because nobody cleared a row.
/// Returns the stored spelling, not a re-derived one: the Mac round-trips this
/// exact string back as its confirmation, and `settled` compares the two by
/// equality (see [`fleet_capture_state`]).
pub fn intent_until(conn: &Connection, now: DateTime<Utc>) -> rusqlite::Result<Option<String>> {
    let Some(raw) = setting(conn, INTENT_KEY)? else {
        return Ok(None);
    };
    if raw.is_empty() {
        return Ok(None);
    }
    let Some(parsed) = crate::instant::parse(&raw) else {
        // Unparseable reads as RUNNING, never as a pause nobody can clear.
        return Ok(None);
    };
    if parsed.with_timezone(&Utc) <= now {
        return Ok(None);
    }
    Ok(crate::instant::python_isoformat(&raw))
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
    let Some(at) = crate::instant::parse(&at) else {
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

/// A pause is BOUNDED, always. The household's control is "stop recording",
/// never "stop recording indefinitely" — a pause that outlives everyone's memory
/// of setting it is how a week of the archive goes missing without anyone
/// deciding to lose it.
fn max_pause() -> Duration {
    Duration::hours(24)
}

/// When a pause starting at `now` must end, clamped to [`max_pause`].
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

/// Record a bounded pause as the fleet's DESIRED state, and return its resume-by
/// in the spelling that will be stored and compared.
///
/// ⚠ This is intent, not actuation. Isis runs no capture agent: the Mac's mirror
/// polls this and applies it, then reports back — which is why
/// [`fleet_capture_state`] serves confirmed and desired separately instead of
/// pretending the press already took effect.
pub fn intent_pause(
    conn: &Connection,
    now: DateTime<Utc>,
    minutes: Option<i64>,
) -> rusqlite::Result<String> {
    let until = compute_resume_by(now, minutes);
    let iso = crate::instant::python_isoformat_utc(until);
    set_setting(conn, INTENT_KEY, &iso)?;
    Ok(iso)
}

/// Record "run" as the fleet's desired state.
///
/// ⚠ Written as EMPTY rather than deleted, so a resume is a value the mirror can
/// read and act on. A missing row and a cleared one already mean the same thing
/// to [`intent_until`]; keeping the row means a reader never has to tell "never
/// paused" from "resumed".
pub fn intent_resume(conn: &Connection) -> rusqlite::Result<()> {
    set_setting(conn, INTENT_KEY, "")
}

/// Append the audit record of WHO asked for a pause or resume.
///
/// Capture control is login-free on the recording plane, so the agent's own
/// PAUSE/RESUME event cannot name a caller; this carries the request's origin
/// descriptor instead.
///
/// ⚠ Best-effort by contract: the caller must not let a failed audit fail the
/// control action. Silencing a household's microphone must never depend on a
/// bookkeeping write succeeding.
pub fn record_control_origin(
    conn: &Connection,
    now: DateTime<Utc>,
    verb: &str,
    origin: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO capture_events (utc, kind, source_id, detail) VALUES (?1, ?2, NULL, ?3)",
        rusqlite::params![
            crate::instant::python_isoformat_utc(now),
            "control_request",
            format!("{verb} — {origin}"),
        ],
    )?;
    Ok(())
}

// --- the HTTP surface -------------------------------------------------------

use crate::reads::State as ReadState;
use crate::route;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use std::sync::Arc;

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

/// `GET /api/capture` — is the household being recorded, and if paused, until
/// when.
///
/// ⚠ The long-poll is what makes a press propagate in ~RTT instead of a poll
/// interval, and it is load-bearing for the Mac's mirror: that exchange hangs
/// here while its intent is unchanged, so the hang doubles as the mirror's
/// pacing. Answering immediately would not break correctness, it would turn
/// every recorder in the house into a 5-second poller.
pub async fn status_route(
    State(st): State<Arc<ReadState>>,
    Query(q): Query<StatusQuery>,
) -> Response {
    let root = st.root.clone();
    let wait = q.wait.clamp(0.0, WAIT_CAP.as_secs_f64());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(wait);

    loop {
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
        // ⚠ Sleep rather than wait on a condition variable. The Python parks on
        // an in-process notify, which works because ONE process serves every
        // request; here the writer may be the Python tier during the cutover,
        // and a notify it cannot send would hang this until the cap. Polling a
        // 2s slice costs one cheap read and cannot miss a change from either
        // side.
        tokio::time::sleep(WAIT_SLICE.min(deadline - std::time::Instant::now())).await;
    }
}
