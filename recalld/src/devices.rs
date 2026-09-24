//! What each recorder says about itself: mic heartbeats and upload outboxes.
//!
//! Both write endpoints are unauthenticated by design, and both are status,
//! never control. A phone on an older build must cost its own detail and
//! nothing else, so an unparseable time is dropped rather than refused. `at` is
//! the server's clock: a beat is evidence the app reached the fleet just now,
//! whatever the phone's clock says. To probe reachability, GET the path (a 401
//! writes nothing); a POST leaves a row. Each list is one JSON value in
//! `settings`, rewritten whole, so two beats arriving together can lose one,
//! which an hourly status tolerates.

use audiocore::instant;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

const BEATS_KEY: &str = "device_mic_heartbeats";
const REPORTS_KEY: &str = "device_outbox_reports";

/// All client-supplied and all bound: they land in one JSON value and are
/// rendered into a health check.
const MAX_DEVICE_LEN: usize = 64;
const MAX_TEXT_LEN: usize = 64;
const MAX_REASON_LEN: usize = 200;

/// How often an app is asked to beat. The canonical declaration: the Android
/// app's `Heartbeat.EVERY_MINUTES` and fleetwatch's thresholds
/// (`xinutec-infra/mac-mini/recall_mics.py`) are pinned to it. `MAX_AGE_DAYS`
/// is deliberately not a multiple of it: when a silent phone stops being a
/// device is a policy, not a count of missed beats.
pub const BEAT_EVERY_MINUTES: i64 = 60;

/// The write endpoint is unauthenticated, so the number of devices is
/// client-controlled; the cap turns a stray row into eviction.
const MAX_DEVICES: usize = 16;

/// How long a silent device stays in the list: it is last-known status, not a
/// registry, and the count cap alone removes nothing while the list is short.
const MAX_AGE_DAYS: i64 = 30;

/// Truncate by character, never by byte: a cut mid-character panics.
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
    // A blank value is unset.
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
/// object. Empty rather than a 500, because this is a health endpoint's request
/// path; logged, because the next write would persist the loss of every
/// device's last beat.
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

/// A stored instant in the archive's spelling; a naive timestamp is UTC.
fn when(value: Option<&serde_json::Value>) -> Option<String> {
    let text = match value {
        None | Some(serde_json::Value::Null) => return None,
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    };
    instant::respell_utc(&text)
}

fn text_of(value: Option<&serde_json::Value>, max: usize) -> String {
    match value {
        None | Some(serde_json::Value::Null) => String::new(),
        Some(serde_json::Value::String(s)) => clip(s, max),
        Some(other) => clip(&other.to_string(), max),
    }
}

/// `None` stays `None`; anything else takes JSON truthiness.
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

/// Forget one device's row: the way to undo a stray write, and the one
/// operation here that destroys a reading rather than replacing it, which is
/// why its route is gated. Returns whether the device was there.
pub fn forget(conn: &Connection, key: &str, device: &str) -> rusqlite::Result<bool> {
    let mut map = stored_map(conn, key)?;
    if map.remove(device).is_none() {
        return Ok(false);
    }
    set_setting(
        conn,
        key,
        &crate::pyjson::dump(&serde_json::Value::Object(map)),
    )?;
    Ok(true)
}

/// The two keys a device may be forgotten from.
pub const BEATS: &str = BEATS_KEY;
pub const REPORTS: &str = REPORTS_KEY;

/// Every app's last beat, oldest device id first. Never fails on a bad entry.
pub fn read_beats(conn: &Connection) -> rusqlite::Result<Vec<Beat>> {
    let map = stored_map(conn, BEATS_KEY)?;
    // Sorted by device id, so the payload is stable.
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

/// Keep the most recently heard devices, ranked by the stored `at` text. An
/// entry that cannot be read sorts oldest and is evicted first, so a malformed
/// row is self-clearing.
fn evicted(
    mut entries: serde_json::Map<String, serde_json::Value>,
    now: DateTime<Utc>,
) -> serde_json::Map<String, serde_json::Value> {
    // Age first, so a device that aged out does not occupy one of the slots the
    // count cap is deciding between.
    let cutoff = now - chrono::Duration::days(MAX_AGE_DAYS);
    entries.retain(|_, value| {
        value
            .as_object()
            .and_then(|o| o.get("at"))
            .and_then(serde_json::Value::as_str)
            .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
            // An unparseable `at` is left to the count cap, which sorts it
            // oldest: an unreadable row is worth seeing before it goes.
            .is_none_or(|at| at.with_timezone(&Utc) >= cutoff)
    });
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
    // Descending by `at`, stable, and rebuilt in ranked order: the value is
    // stored as text, so the order is part of what is written.
    ranked.sort_by(|a, b| b.1.cmp(&a.1));
    let mut kept = serde_json::Map::with_capacity(MAX_DEVICES);
    for (device, _) in ranked.into_iter().take(MAX_DEVICES) {
        if let Some(value) = entries.remove(&device) {
            kept.insert(device, value);
        }
    }
    kept
}

/// Store this app's beat, replacing whatever it said before.
pub fn record_beat(conn: &Connection, beat: &Beat, now: DateTime<Utc>) -> rusqlite::Result<()> {
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
        &crate::pyjson::dump(&serde_json::Value::Object(evicted(map, now))),
    )
}

/// Store this phone's report, replacing whatever it said before. Evicted on
/// the same terms as the beats.
pub fn record_report(
    conn: &Connection,
    report: &Report,
    now: DateTime<Utc>,
) -> rusqlite::Result<()> {
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
        &crate::pyjson::dump(&serde_json::Value::Object(evicted(map, now))),
    )
}

// --- HTTP -------------------------------------------------------------------

use crate::{reads, route, work};
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

/// Every field but `device` is optional: an app on an older build must still
/// count as alive. The beat arriving is the signal; the rest is detail.
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

/// A client-supplied instant, or `None` if it will not parse. Dropped, never
/// refused: the beat is what matters.
fn client_instant(value: Option<&str>) -> Option<String> {
    instant::respell_utc(value?)
}

pub async fn heartbeat_post_route(
    State(st): State<Arc<reads::State>>,
    Json(body): Json<HeartbeatIn>,
) -> Response {
    let root = st.root.clone();
    // The SERVER's clock. See the module note.
    let at = instant::python_isoformat_utc(Utc::now());
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
        record_beat(&work::open_write(&root)?, &beat, Utc::now())
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
    let at = instant::python_isoformat_utc(Utc::now());
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
        record_report(&work::open_write(&root)?, &report, Utc::now())
    })
    .await
    {
        Ok(()) => route::ack(),
        Err(response) => response,
    }
}

/// The heartbeats as the wire carries them.
pub fn beats_out(conn: &Connection) -> rusqlite::Result<BeatsOut> {
    Ok(BeatsOut {
        items: read_beats(conn)?
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
}

/// The outbox reports as the wire carries them. Shared by both planes — see
/// [`beats_out`].
pub fn reports_out(conn: &Connection) -> rusqlite::Result<ReportsOut> {
    Ok(ReportsOut {
        items: read_reports(conn)?
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
}

pub async fn heartbeat_get_route(State(st): State<Arc<reads::State>>) -> Response {
    let root = st.root.clone();
    route::json("heartbeats", move || beats_out(&reads::open(&root)?)).await
}

pub async fn outbox_get_route(State(st): State<Arc<reads::State>>) -> Response {
    let root = st.root.clone();
    route::json("outboxes", move || reports_out(&reads::open(&root)?)).await
}

/// Forget one device's heartbeat. Gated, unlike the POST beside it: forgetting
/// is a person's act.
pub async fn heartbeat_forget_route(
    State(st): State<Arc<reads::State>>,
    axum::extract::Path(device): axum::extract::Path<String>,
) -> Response {
    forget_route(st, BEATS, device, "forget heartbeat").await
}

pub async fn outbox_forget_route(
    State(st): State<Arc<reads::State>>,
    axum::extract::Path(device): axum::extract::Path<String>,
) -> Response {
    forget_route(st, REPORTS, device, "forget outbox").await
}

async fn forget_route(
    st: Arc<reads::State>,
    key: &'static str,
    device: String,
    what: &'static str,
) -> Response {
    let root = st.root.clone();
    match route::blocking(what, move || {
        forget(&work::open_write(&root)?, key, &device)
    })
    .await
    {
        // 404 for a device that was not there, so a typo reads as a typo rather
        // than as a successful removal of nothing.
        Ok(true) => route::ack(),
        Ok(false) => (StatusCode::NOT_FOUND, "no such device").into_response(),
        Err(response) => response,
    }
}
