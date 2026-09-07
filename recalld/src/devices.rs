//! What each recorder says about itself: mic heartbeats and upload outboxes.
//! Ported from `recall.api_devices`, `recall.mic_alive` and `recall.outbox`.
//!
//! ⚠ **Both endpoints are unauthenticated by design**, and both are STATUS, never
//! control. A phone on an older build must cost its own line and nothing else, so
//! an unparseable time is DROPPED rather than refused — the beat itself is the
//! part that matters, and refusing it would delete the signal.
//!
//! ⚠ **`at` is the SERVER's clock, never the phone's.** A beat is evidence that
//! this app reached the fleet just now. A phone with a wrong clock would
//! otherwise report itself permanently fresh, or permanently stale. The phone's
//! own times are kept only where they say something about the phone
//! (`startedAt`, `oldestQueuedAt`).
//!
//! ⚠ **Stored as one JSON value per key in `settings`**, rewritten whole. That is
//! a read-modify-write, so two beats arriving together can lose one — which is
//! how the Python has always worked and is acceptable for an hourly status that
//! is rewritten wholesale.

use crate::instant;
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

const BEATS_KEY: &str = "device_mic_heartbeats";
const REPORTS_KEY: &str = "device_outbox_reports";

/// All client-supplied and all bound: they land in one JSON value and are
/// rendered into a health check.
const MAX_DEVICE_LEN: usize = 64;
const MAX_TEXT_LEN: usize = 64;
const MAX_REASON_LEN: usize = 200;

/// ⚠ The write endpoint is unauthenticated, so the number of devices is
/// client-controlled. A single test post once put a stray row into the fleet's
/// setting that had to be removed by hand with sqlite3 inside the pod; the cap
/// turns that from surgery into eviction.
const MAX_DEVICES: usize = 16;

/// Truncate by CHARACTER, as Python's `value[:n]` does.
///
/// ⚠ Not by byte. A device name with any non-ASCII character would otherwise be
/// cut at a different point, and a cut landing mid-character panics.
fn clip(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

/// One mic app saying it is still there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Beat {
    pub device: String,
    pub app: String,
    pub version: String,
    pub started_at: Option<String>,
    pub streaming: bool,
    pub charging: Option<bool>,
    pub mic_ok: Option<bool>,
    pub via_lan: Option<bool>,
    pub at: String,
}

/// One phone's outbox, as it last described it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub device: String,
    pub queued: i64,
    pub oldest_queued_at: Option<String>,
    pub failing: i64,
    pub reason: Option<String>,
    pub at: String,
}

fn get_setting(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    let raw: Option<String> = conn
        .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
            r.get(0)
        })
        .optional()?;
    // The Python treats a blank value as unset (`return value or None`).
    Ok(raw.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty()))
}

fn set_setting(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        (key, value),
    )?;
    Ok(())
}

/// The stored map, or empty when it is missing, blank, unparseable, or not an
/// object.
///
/// ⚠ **Degrading to empty is deliberate and it is LOUD.** This is read on a
/// health endpoint's request path, so a half-written value must not blank the
/// whole answer with a 500 — that is the Python's rule and the reason the beats
/// exist at all. But losing the blob silently would discard EVERY device's last
/// beat with no trace, and the next write would persist that loss. So it is
/// logged: degraded reads are a fault to notice, not a shape to accept quietly.
fn stored_map(
    conn: &Connection,
    key: &str,
) -> rusqlite::Result<serde_json::Map<String, serde_json::Value>> {
    let Some(raw) = get_setting(conn, key)? else {
        return Ok(serde_json::Map::new());
    };
    match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(serde_json::Value::Object(map)) => Ok(map),
        Ok(other) => {
            tracing::warn!(
                "{key} holds {} rather than an object; {} bytes of device status \
                 are being read as empty",
                match other {
                    serde_json::Value::Null => "null",
                    serde_json::Value::Bool(_) => "a bool",
                    serde_json::Value::Number(_) => "a number",
                    serde_json::Value::String(_) => "a string",
                    serde_json::Value::Array(_) => "an array",
                    serde_json::Value::Object(_) => unreachable!("matched above"),
                },
                raw.len()
            );
            Ok(serde_json::Map::new())
        }
        Err(err) => {
            tracing::warn!(
                "{key} will not parse ({err}); {} bytes of device status are being \
                 read as empty and the next write will overwrite them",
                raw.len()
            );
            Ok(serde_json::Map::new())
        }
    }
}

/// A stored instant, normalised the way the Python's `_when` does.
///
/// ⚠ **A naive timestamp means UTC and must be STAMPED, not converted.**
/// `.astimezone(UTC)` reads a naive value as LOCAL time, so a phone on a build
/// that sends no offset would have every beat shifted by the host's offset — an
/// hour in summer — and read as older than it is, moving a stuck upload back
/// under the threshold that exists to notice it.
fn when(value: Option<&serde_json::Value>) -> Option<String> {
    let text = match value {
        None | Some(serde_json::Value::Null) => return None,
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    };
    instant::python_isoformat(&text)
}

fn text_of(value: Option<&serde_json::Value>, max: usize) -> String {
    match value {
        None | Some(serde_json::Value::Null) => String::new(),
        Some(serde_json::Value::String(s)) => clip(s, max),
        Some(other) => clip(&other.to_string(), max),
    }
}

/// `None` stays `None`; anything else takes Python's truthiness.
fn flag(value: Option<&serde_json::Value>) -> Option<bool> {
    match value {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::Bool(b)) => Some(*b),
        Some(serde_json::Value::Number(n)) => Some(n.as_f64().is_some_and(|f| f != 0.0)),
        Some(serde_json::Value::String(s)) => Some(!s.is_empty()),
        Some(_) => Some(true),
    }
}

fn one_beat(device: &str, raw: &serde_json::Value) -> Option<Beat> {
    let raw = raw.as_object()?;
    // `at` and `streaming` are REQUIRED; their absence costs this device its line.
    let at = when(raw.get("at"))?;
    let streaming = flag(raw.get("streaming"))?;
    Some(Beat {
        device: clip(device, MAX_DEVICE_LEN),
        app: text_of(raw.get("app"), MAX_TEXT_LEN),
        version: text_of(raw.get("version"), MAX_TEXT_LEN),
        started_at: when(raw.get("startedAt")),
        streaming,
        charging: flag(raw.get("charging")),
        mic_ok: flag(raw.get("micOk")),
        via_lan: flag(raw.get("viaLan")),
        at,
    })
}

fn one_report(device: &str, raw: &serde_json::Value) -> Option<Report> {
    let raw = raw.as_object()?;
    let at = when(raw.get("at"))?;
    Some(Report {
        device: clip(device, MAX_DEVICE_LEN),
        queued: raw.get("queued")?.as_i64()?,
        oldest_queued_at: when(raw.get("oldestQueuedAt")),
        failing: raw.get("failing")?.as_i64()?,
        reason: match raw.get("reason") {
            None | Some(serde_json::Value::Null) => None,
            Some(other) => Some(text_of(Some(other), MAX_REASON_LEN)),
        },
        at,
    })
}

/// Every app's last beat, oldest device id first. Never fails on a bad entry.
pub fn read_beats(conn: &Connection) -> rusqlite::Result<Vec<Beat>> {
    let map = stored_map(conn, BEATS_KEY)?;
    // serde_json's Map preserves insertion order; the Python sorts by device id.
    let mut devices: Vec<&String> = map.keys().collect();
    devices.sort();
    Ok(devices
        .into_iter()
        .filter_map(|d| one_beat(d, &map[d]))
        .collect())
}

pub fn read_reports(conn: &Connection) -> rusqlite::Result<Vec<Report>> {
    let map = stored_map(conn, REPORTS_KEY)?;
    let mut devices: Vec<&String> = map.keys().collect();
    devices.sort();
    Ok(devices
        .into_iter()
        .filter_map(|d| one_report(d, &map[d]))
        .collect())
}

/// Keep the most recently heard devices, ranked by the STORED `at` text.
///
/// ⚠ An entry that cannot be read at all sorts oldest and is evicted first,
/// which is what makes a malformed row self-clearing rather than permanent.
fn evicted(
    mut entries: serde_json::Map<String, serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    if entries.len() <= MAX_DEVICES {
        return entries;
    }
    let mut ranked: Vec<(String, String)> = entries
        .iter()
        .map(|(k, v)| {
            let at = v
                .as_object()
                .and_then(|o| o.get("at"))
                .and_then(|a| a.as_str())
                .unwrap_or_default()
                .to_owned();
            (k.clone(), at)
        })
        .collect();
    // Descending by `at`; Python's sort is stable, so ties keep insertion order.
    ranked.sort_by(|a, b| b.1.cmp(&a.1));
    // ⚠ Rebuilt in RANKED order, not filtered in place. Python's `dict(ranked[:n])`
    // produces a map ordered most-recent-first, and this value is stored as TEXT —
    // retaining the original order writes a different string for the same
    // surviving set.
    let mut kept = serde_json::Map::with_capacity(MAX_DEVICES);
    for (device, _) in ranked.into_iter().take(MAX_DEVICES) {
        if let Some(value) = entries.remove(&device) {
            kept.insert(device, value);
        }
    }
    kept
}

/// Store this app's beat, replacing whatever it said before.
pub fn record_beat(conn: &Connection, beat: &Beat) -> rusqlite::Result<()> {
    let mut map = stored_map(conn, BEATS_KEY)?;
    map.insert(
        clip(&beat.device, MAX_DEVICE_LEN),
        serde_json::json!({
            "app": clip(&beat.app, MAX_TEXT_LEN),
            "version": clip(&beat.version, MAX_TEXT_LEN),
            "startedAt": beat.started_at,
            "streaming": beat.streaming,
            "charging": beat.charging,
            "micOk": beat.mic_ok,
            "viaLan": beat.via_lan,
            "at": beat.at,
        }),
    );
    set_setting(
        conn,
        BEATS_KEY,
        &crate::pyjson::dump(&serde_json::Value::Object(evicted(map))),
    )
}

/// Store this phone's report, replacing whatever it said before.
///
/// ⚠ No eviction here, unlike the beats — mirroring the Python rather than
/// improving on it. That asymmetry is #1408, and fixing it belongs with that
/// issue rather than smuggled into a port.
pub fn record_report(conn: &Connection, report: &Report) -> rusqlite::Result<()> {
    let mut map = stored_map(conn, REPORTS_KEY)?;
    map.insert(
        clip(&report.device, MAX_DEVICE_LEN),
        serde_json::json!({
            "queued": report.queued,
            "oldestQueuedAt": report.oldest_queued_at,
            "failing": report.failing,
            "reason": report.reason.as_deref().map(|r| clip(r, MAX_REASON_LEN)),
            "at": report.at,
        }),
    );
    set_setting(
        conn,
        REPORTS_KEY,
        &crate::pyjson::dump(&serde_json::Value::Object(map)),
    )
}

// --- HTTP -------------------------------------------------------------------

use crate::{reads, route, work};
use axum::Json;
use axum::extract::State;
use axum::response::Response;
use std::sync::Arc;

/// ⚠ **Every field but `device` is OPTIONAL, and that is the whole design.** An
/// app on an older build must still count as alive: the beat arriving is the
/// signal, and the rest is detail for the reader once it stops arriving. Requiring
/// `app`, `version` or `streaming` would 422 exactly the phone this endpoint
/// exists to notice.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatIn {
    device: String,
    #[serde(default)]
    app: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    streaming: bool,
    #[serde(default)]
    charging: Option<bool>,
    #[serde(default)]
    mic_ok: Option<bool>,
    #[serde(default)]
    via_lan: Option<bool>,
}

/// Optional for the same reason as [`HeartbeatIn`]: a report only sent on failure
/// would leave the last bad reading standing after the queue drained.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutboxIn {
    device: String,
    #[serde(default)]
    queued: i64,
    #[serde(default)]
    oldest_queued_at: Option<String>,
    #[serde(default)]
    failing: i64,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BeatOut {
    device: String,
    app: String,
    version: String,
    started_at: Option<String>,
    streaming: bool,
    charging: Option<bool>,
    mic_ok: Option<bool>,
    via_lan: Option<bool>,
    at: String,
}

#[derive(Serialize)]
pub struct BeatsOut {
    items: Vec<BeatOut>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportOut {
    device: String,
    queued: i64,
    oldest_queued_at: Option<String>,
    failing: i64,
    reason: Option<String>,
    at: String,
}

#[derive(Serialize)]
pub struct ReportsOut {
    items: Vec<ReportOut>,
}

/// A client-supplied instant, or `None` if it will not parse.
///
/// ⚠ Dropped, never refused. An app on an older build should cost its own detail
/// and nothing else — least of all the beat itself, which is the part that
/// matters.
fn client_instant(value: Option<&str>) -> Option<String> {
    instant::python_isoformat(value?)
}

pub async fn heartbeat_post_route(
    State(st): State<Arc<reads::State>>,
    Json(body): Json<HeartbeatIn>,
) -> Response {
    let root = st.root.clone();
    // The SERVER's clock. See the module note.
    let at = instant::python_isoformat(&Utc::now().to_rfc3339()).unwrap_or_default();
    let beat = Beat {
        device: body.device,
        app: body.app,
        version: body.version,
        started_at: client_instant(body.started_at.as_deref()),
        streaming: body.streaming,
        charging: body.charging,
        mic_ok: body.mic_ok,
        via_lan: body.via_lan,
        at,
    };
    match route::blocking("heartbeat", move || {
        record_beat(&work::open_write(&root)?, &beat)
    })
    .await
    {
        Ok(()) => route::ack(),
        Err(response) => response,
    }
}

pub async fn outbox_post_route(
    State(st): State<Arc<reads::State>>,
    Json(body): Json<OutboxIn>,
) -> Response {
    let root = st.root.clone();
    let at = instant::python_isoformat(&Utc::now().to_rfc3339()).unwrap_or_default();
    let report = Report {
        device: body.device,
        // Clamped, not refused: a negative count is a client bug, not a reason to
        // lose the report.
        queued: body.queued.max(0),
        oldest_queued_at: client_instant(body.oldest_queued_at.as_deref()),
        failing: body.failing.max(0),
        reason: body.reason,
        at,
    };
    match route::blocking("outbox report", move || {
        record_report(&work::open_write(&root)?, &report)
    })
    .await
    {
        Ok(()) => route::ack(),
        Err(response) => response,
    }
}

pub async fn heartbeat_get_route(State(st): State<Arc<reads::State>>) -> Response {
    let root = st.root.clone();
    route::json("heartbeats", move || {
        let beats = read_beats(&reads::open(&root)?)?;
        Ok(BeatsOut {
            items: beats
                .into_iter()
                .map(|b| BeatOut {
                    device: b.device,
                    app: b.app,
                    version: b.version,
                    started_at: b.started_at,
                    streaming: b.streaming,
                    charging: b.charging,
                    mic_ok: b.mic_ok,
                    via_lan: b.via_lan,
                    at: b.at,
                })
                .collect(),
        })
    })
    .await
}

pub async fn outbox_get_route(State(st): State<Arc<reads::State>>) -> Response {
    let root = st.root.clone();
    route::json("outboxes", move || {
        let reports = read_reports(&reads::open(&root)?)?;
        Ok(ReportsOut {
            items: reports
                .into_iter()
                .map(|r| ReportOut {
                    device: r.device,
                    queued: r.queued,
                    oldest_queued_at: r.oldest_queued_at,
                    failing: r.failing,
                    reason: r.reason,
                    at: r.at,
                })
                .collect(),
        })
    })
    .await
}
