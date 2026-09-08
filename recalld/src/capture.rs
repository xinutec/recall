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
