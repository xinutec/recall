//! What each recorder says about itself: mic heartbeats and upload outboxes.
//!
//! - The write endpoints are unauthenticated: status, never control. An older
//!   app's unparseable field is dropped, not refused.
//! - `at` is the server's clock: the beat proves the app reached the fleet now.
//! - To probe reachability, GET (a 401 writes nothing); a POST leaves a row.
//! - Each list is one JSON value in `settings`, rewritten whole, so two
//!   simultaneous beats can lose one; an hourly status tolerates that.

use audiocore::devices::Heartbeat;
use audiocore::instant;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

crate::statements! {
    SETTING: Meaning =
        "SELECT value FROM settings WHERE key = ?1";
    SET_SETTING: Meaning =
        "INSERT INTO settings (key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value";
}

/// The `settings` keys the two lists live under.
pub const BEATS: &str = "device_mic_heartbeats";
pub const REPORTS: &str = "device_outbox_reports";

/// Client-supplied text is bounded: it is stored and shown in a health check.
const MAX_DEVICE_LEN: usize = 64;
const MAX_TEXT_LEN: usize = 64;
const MAX_REASON_LEN: usize = 200;

/// How often an app beats. Android's `Heartbeat.EVERY_MINUTES` and fleetwatch's
/// thresholds (`xinutec-infra/mac-mini/recall_mics.py`) are pinned to it.
pub const BEAT_EVERY_MINUTES: i64 = 60;

/// Anyone can write, so the device count is capped; a stray row is evicted.
const MAX_DEVICES: usize = 16;

/// How long a silent device stays listed.
const MAX_AGE_DAYS: i64 = 30;

/// Truncate by character: a byte cut can panic mid-character.
fn clip(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Beat {
    pub device: String,
    pub app: String,
    pub version: String,
    pub started_at: Option<String>,
    pub streaming: bool,
    pub charging: Option<bool>,
    pub mic_ok: Option<bool>,
    pub via_lan: Option<bool>,
    /// Audio the phone captured but could not stream, since its previous beat.
    pub dropped_bytes: Option<i64>,
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub device: String,
    pub queued: i64,
    pub oldest_queued_at: Option<String>,
    pub failing: i64,
    pub reason: Option<String>,
    pub at: String,
}

fn get_setting(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    let raw: Option<String> = SETTING.query_row(conn, [key], |r| r.get(0)).optional()?;
    Ok(raw.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty()))
}

fn set_setting(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    SET_SETTING.execute(conn, (key, value))?;
    Ok(())
}

/// A beat as stored, under its device's id: [`Beat`] without the key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredBeat {
    app: String,
    version: String,
    started_at: Option<String>,
    streaming: bool,
    charging: Option<bool>,
    mic_ok: Option<bool>,
    via_lan: Option<bool>,
    dropped_bytes: Option<i64>,
    at: String,
}

/// A report as stored, under its device's id: [`Report`] without the key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredReport {
    queued: i64,
    oldest_queued_at: Option<String>,
    failing: i64,
    reason: Option<String>,
    at: String,
}

/// The stored map, or empty if it is missing or not a map: a health endpoint
/// must not 500. Logged, since the next write replaces it. Entries stay raw:
/// one that will not read survives the next write, to be noticed.
fn stored(conn: &Connection, key: &str) -> rusqlite::Result<BTreeMap<String, serde_json::Value>> {
    let Some(raw) = get_setting(conn, key)? else {
        return Ok(BTreeMap::new());
    };
    match serde_json::from_str(&raw) {
        Ok(entries) => Ok(entries),
        Err(err) => {
            tracing::warn!(
                "{key} will not parse as a map ({err}); {} bytes of device status are \
                 being read as empty and the next write will overwrite them",
                raw.len()
            );
            Ok(BTreeMap::new())
        }
    }
}

/// The entries that read as `T`; one that does not is logged and skipped.
fn readable<T: serde::de::DeserializeOwned>(
    key: &str,
    entries: BTreeMap<String, serde_json::Value>,
) -> impl Iterator<Item = (String, T)> {
    entries
        .into_iter()
        .filter_map(move |(device, value)| match serde_json::from_value(value) {
            Ok(entry) => Some((device, entry)),
            Err(err) => {
                tracing::warn!("{key}: {device}'s entry will not read ({err}); skipped");
                None
            }
        })
}

fn store(
    conn: &Connection,
    key: &str,
    entries: &BTreeMap<String, serde_json::Value>,
) -> rusqlite::Result<()> {
    let json = serde_json::to_string(entries).expect("a map of JSON values serialises");
    set_setting(conn, key, &json)
}

/// Forget one device's row, to undo a stray write. Returns whether it was
/// there.
pub fn forget(conn: &Connection, key: &str, device: &str) -> rusqlite::Result<bool> {
    let mut map = stored(conn, key)?;
    if map.remove(device).is_none() {
        return Ok(false);
    }
    store(conn, key, &map)?;
    Ok(true)
}

/// Every app's last beat, by device id.
pub fn read_beats(conn: &Connection) -> rusqlite::Result<Vec<Beat>> {
    Ok(readable::<StoredBeat>(BEATS, stored(conn, BEATS)?)
        .map(|(device, b)| Beat {
            device,
            app: b.app,
            version: b.version,
            started_at: b.started_at,
            streaming: b.streaming,
            charging: b.charging,
            mic_ok: b.mic_ok,
            via_lan: b.via_lan,
            dropped_bytes: b.dropped_bytes,
            at: b.at,
        })
        .collect())
}

pub fn read_reports(conn: &Connection) -> rusqlite::Result<Vec<Report>> {
    Ok(readable::<StoredReport>(REPORTS, stored(conn, REPORTS)?)
        .map(|(device, r)| Report {
            device,
            queued: r.queued,
            oldest_queued_at: r.oldest_queued_at,
            failing: r.failing,
            reason: r.reason,
            at: r.at,
        })
        .collect())
}

/// Drop devices silent past `MAX_AGE_DAYS`, then keep the `MAX_DEVICES` most
/// recent by stored `at` text. An unreadable `at` sorts oldest.
fn evicted(
    mut entries: BTreeMap<String, serde_json::Value>,
    now: DateTime<Utc>,
) -> BTreeMap<String, serde_json::Value> {
    let at = |entry: &serde_json::Value| {
        entry
            .get("at")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let cutoff = now - chrono::Duration::days(MAX_AGE_DAYS);
    entries.retain(|_, entry| {
        DateTime::parse_from_rfc3339(&at(entry))
            .ok()
            // Unparseable: left to the count cap, so it is seen before it goes.
            .is_none_or(|when| when.with_timezone(&Utc) >= cutoff)
    });
    if entries.len() <= MAX_DEVICES {
        return entries;
    }
    let mut ranked: Vec<(String, serde_json::Value)> = entries.into_iter().collect();
    ranked.sort_by_cached_key(|(_, entry)| std::cmp::Reverse(at(entry)));
    ranked.into_iter().take(MAX_DEVICES).collect()
}

/// An entry as stored: a plain struct of strings, numbers and options.
fn entry(value: &impl Serialize) -> serde_json::Value {
    serde_json::to_value(value).expect("a stored entry serialises")
}

pub fn record_beat(conn: &Connection, beat: &Beat, now: DateTime<Utc>) -> rusqlite::Result<()> {
    let mut map = stored(conn, BEATS)?;
    map.insert(
        clip(&beat.device, MAX_DEVICE_LEN),
        entry(&StoredBeat {
            app: clip(&beat.app, MAX_TEXT_LEN),
            version: clip(&beat.version, MAX_TEXT_LEN),
            started_at: beat.started_at.clone(),
            streaming: beat.streaming,
            charging: beat.charging,
            mic_ok: beat.mic_ok,
            via_lan: beat.via_lan,
            dropped_bytes: beat.dropped_bytes,
            at: beat.at.clone(),
        }),
    );
    store(conn, BEATS, &evicted(map, now))
}

pub fn record_report(
    conn: &Connection,
    report: &Report,
    now: DateTime<Utc>,
) -> rusqlite::Result<()> {
    let mut map = stored(conn, REPORTS)?;
    map.insert(
        clip(&report.device, MAX_DEVICE_LEN),
        entry(&StoredReport {
            queued: report.queued,
            oldest_queued_at: report.oldest_queued_at.clone(),
            failing: report.failing,
            reason: report.reason.as_deref().map(|r| clip(r, MAX_REASON_LEN)),
            at: report.at.clone(),
        }),
    );
    store(conn, REPORTS, &evicted(map, now))
}

// --- HTTP -------------------------------------------------------------------

use crate::{reads, route, work};
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

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
pub struct BeatsOut {
    items: Vec<Beat>,
}

#[derive(Serialize)]
pub struct ReportsOut {
    items: Vec<Report>,
}

fn client_instant(value: Option<&str>) -> Option<String> {
    instant::respell_utc(value?)
}

pub async fn heartbeat_post_route(
    State(st): State<Arc<reads::State>>,
    Json(body): Json<Heartbeat>,
) -> Response {
    let root = st.root.clone();
    let at = instant::python_isoformat_utc(Utc::now());
    let beat = Beat {
        device: body.device,
        app: body.app.unwrap_or_default(),
        version: body.version.unwrap_or_default(),
        started_at: client_instant(body.started_at.as_deref()),
        streaming: body.streaming.unwrap_or(false),
        charging: body.charging,
        mic_ok: body.mic_ok,
        via_lan: body.via_lan,
        // A negative count is a client bug: clamped, not refused.
        dropped_bytes: body.dropped_bytes.map(|b| b.max(0)),
        at,
    };
    route::acked(
        route::blocking("heartbeat", move || {
            record_beat(&work::open_write(&root)?, &beat, Utc::now())
        })
        .await,
    )
}

pub async fn outbox_post_route(
    State(st): State<Arc<reads::State>>,
    Json(body): Json<OutboxIn>,
) -> Response {
    let root = st.root.clone();
    let at = instant::python_isoformat_utc(Utc::now());
    let report = Report {
        device: body.device,
        // A negative count is a client bug: clamped, not refused.
        queued: body.queued.max(0),
        oldest_queued_at: client_instant(body.oldest_queued_at.as_deref()),
        failing: body.failing.max(0),
        reason: body.reason,
        at,
    };
    route::acked(
        route::blocking("outbox report", move || {
            record_report(&work::open_write(&root)?, &report, Utc::now())
        })
        .await,
    )
}

pub fn beats_out(conn: &Connection) -> rusqlite::Result<BeatsOut> {
    Ok(BeatsOut {
        items: read_beats(conn)?,
    })
}

pub fn reports_out(conn: &Connection) -> rusqlite::Result<ReportsOut> {
    Ok(ReportsOut {
        items: read_reports(conn)?,
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

/// Gated, unlike the POST: forgetting is a person's act.
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
        Ok(true) => route::ack(),
        Ok(false) => (StatusCode::NOT_FOUND, "no such device").into_response(),
        Err(response) => response,
    }
}
