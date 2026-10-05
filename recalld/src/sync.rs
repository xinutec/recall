//! The Mac's sync plane. The Mac is a one-way `WireGuard` peer: it dials the
//! fleet and nothing dials back, so a pause pressed in the web UI reaches the
//! microphone by the Mac long-polling [`capture_route`].
//!
//! The credential is `RECALL_SYNC_TOKEN`, shared by both ends; it grants
//! nothing on the browsing plane. Routes: the capture handshake (audiod), the
//! vocabulary prompt (the runner), the live feed (recall-live), and numbers
//! for the doctor.

use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use subtle::ConstantTimeEq;

/// The secret the Mac presents. Without one the plane is not mounted, so it
/// cannot answer open by accident.
pub struct Gate {
    pub expected: String,
    pub root: std::path::PathBuf,
}

const BEARER: &str = "Bearer ";

#[must_use]
pub fn bearer(header: Option<&str>) -> Option<&str> {
    header?.strip_prefix(BEARER)
}

/// Authorise a sync request in constant time, or say what to answer.
pub fn check(presented: Option<&str>, expected: &str) -> Result<(), (StatusCode, &'static str)> {
    let ok = presented.is_some_and(|p| {
        // `ct_eq` short-circuits on unequal length, which is not secret.
        p.as_bytes().ct_eq(expected.as_bytes()).into()
    });
    if ok {
        Ok(())
    } else {
        Err((StatusCode::UNAUTHORIZED, "bad sync token"))
    }
}

/// What the Mac reports it has applied, each mirror pass.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Applied {
    pub running: bool,
    /// The ISO resume-by it applied; absent while recording.
    #[serde(default)]
    pub paused_until: Option<String>,
    /// Each source's last proved recording time.
    #[serde(default)]
    pub source_liveness: serde_json::Map<String, serde_json::Value>,
    /// Seconds to hang while the intent still equals `known_intent`.
    #[serde(default)]
    pub wait: f64,
    /// The intent the Mac has applied; `None` means running.
    #[serde(default)]
    pub known_intent: Option<String>,
}

/// The intent, for the Mac to mirror onto its pause file.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IntentOut {
    pub paused_until: Option<String>,
}

/// As `GET /api/capture`.
const WAIT_CAP: std::time::Duration = std::time::Duration::from_secs(25);
const WAIT_SLICE: std::time::Duration = std::time::Duration::from_secs(2);

/// `source_liveness` carries instants as strings; the reader drops anything
/// else.
fn all_strings(map: &serde_json::Map<String, serde_json::Value>) -> bool {
    map.values().all(serde_json::Value::is_string)
}

/// `POST /sync/capture`: the Mac reports what it applied and reads back the
/// intent, in one round trip. The report is recorded once, before the hang:
/// recording inside it would keep a Mac that died mid-hang looking alive.
pub async fn capture_route(
    State(st): State<Arc<Gate>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if let Err(refusal) = check(bearer(presented), &st.expected) {
        return refusal.into_response();
    }
    let Ok(body) = serde_json::from_slice::<Applied>(&body) else {
        return (StatusCode::UNPROCESSABLE_ENTITY, "bad capture report").into_response();
    };
    if !all_strings(&body.source_liveness) {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            "sourceLiveness values must be strings",
        )
            .into_response();
    }

    let root = st.root.clone();
    let reported = body.paused_until.clone();
    // Subscribe before each derive, so no press is lost in between.
    let mut watcher = crate::capture::intent_watch();
    let mut reply = match crate::route::blocking("sync capture", move || {
        let conn = crate::work::open_write(&root)?;
        let now = chrono::Utc::now();
        crate::capture::record_reported(
            &conn,
            now,
            body.running,
            reported.as_deref(),
            &body.source_liveness,
        )?;
        Ok(IntentOut {
            paused_until: crate::capture::intent_until(&conn, now)?,
        })
    })
    .await
    {
        Ok(reply) => reply,
        Err(response) => return response,
    };

    let wait = body.wait.clamp(0.0, WAIT_CAP.as_secs_f64());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(wait);
    while wait > 0.0 && reply.paused_until == body.known_intent {
        let now = std::time::Instant::now();
        if now >= deadline {
            break;
        }
        // As `capture::status_route`: notified on a press, re-derived each
        // slice for changes nothing signals.
        crate::capture::wait_intent_changed(watcher, WAIT_SLICE.min(deadline - now)).await;
        watcher = crate::capture::intent_watch();
        let root = st.root.clone();
        reply = match crate::route::blocking("sync capture intent", move || {
            crate::capture::intent_until(&crate::work::open_write(&root)?, chrono::Utc::now())
                .map(|paused_until| IntentOut { paused_until })
        })
        .await
        {
            Ok(reply) => reply,
            Err(response) => return response,
        };
    }
    axum::Json(reply).into_response()
}

/// Authorise, then answer a read of the meaning plane.
async fn gated_read<T, F>(
    st: &Arc<Gate>,
    headers: &axum::http::HeaderMap,
    what: &'static str,
    read: F,
) -> Response
where
    F: FnOnce(&rusqlite::Connection) -> rusqlite::Result<T> + Send + 'static,
    T: serde::Serialize + Send + 'static,
{
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if let Err(refusal) = check(bearer(presented), &st.expected) {
        return refusal.into_response();
    }
    let root = st.root.clone();
    crate::route::json(what, move || read(&crate::reads::open(&root)?)).await
}

/// `GET /sync/vocabulary/prompt`: the glossary as an ASR prompt, for the
/// runner to hand to the model shim.
pub async fn vocabulary_prompt_route(
    State(st): State<Arc<Gate>>,
    headers: axum::http::HeaderMap,
) -> Response {
    gated_read(&st, &headers, "sync vocabulary prompt", |conn| {
        Ok(PromptOut {
            prompt: crate::labels::initial_prompt(conn)?,
        })
    })
    .await
}

/// The windows to measure over; no defaults, the doctor owns them.
#[derive(Deserialize)]
pub struct LiveHealthQuery {
    pub lag_since: String,
    pub window_since: String,
    pub window_until: String,
}

/// `GET /sync/live/health`: the live feed's numbers, for the doctor on the Mac.
pub async fn live_health_route(
    State(st): State<Arc<Gate>>,
    headers: axum::http::HeaderMap,
    Query(q): Query<LiveHealthQuery>,
) -> Response {
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if let Err(refusal) = check(bearer(presented), &st.expected) {
        return refusal.into_response();
    }
    // Parsed here, so a bad bound is a 400 rather than a text comparison
    // against something that only looks like a timestamp.
    let (Some(lag_since), Some(window_since), Some(window_until)) = (
        audiocore::instant::parse(&q.lag_since),
        audiocore::instant::parse(&q.window_since),
        audiocore::instant::parse(&q.window_until),
    ) else {
        return (StatusCode::BAD_REQUEST, "unparseable window bound").into_response();
    };
    let root = st.root.clone();
    crate::route::json("sync live health", move || {
        crate::live_tier::live_health(
            &root,
            lag_since.into(),
            window_since.into(),
            window_until.into(),
        )
    })
    .await
}

/// The window `GET /sync/record/health` measures.
#[derive(Deserialize)]
pub struct RecordHealthQuery {
    pub since: String,
}

/// `GET /sync/record/health`: server faults and doubled minutes since `since`.
pub async fn record_health_route(
    State(st): State<Arc<Gate>>,
    headers: axum::http::HeaderMap,
    Query(q): Query<RecordHealthQuery>,
) -> Response {
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if let Err(refusal) = check(bearer(presented), &st.expected) {
        return refusal.into_response();
    }
    let Some(since) = audiocore::instant::parse(&q.since) else {
        return (StatusCode::BAD_REQUEST, "unparseable window bound").into_response();
    };
    let root = st.root.clone();
    crate::route::json("sync record health", move || {
        crate::record_health::measure(&root, since.into())
    })
    .await
}

/// The window `GET /sync/heard` measures.
#[derive(Deserialize)]
pub struct HeardQuery {
    pub since: String,
    pub until: String,
}

/// `GET /sync/heard`: per device source, audio delivered and speech in it, for
/// the doctor's deaf-microphone check.
pub async fn heard_route(
    State(st): State<Arc<Gate>>,
    headers: axum::http::HeaderMap,
    Query(q): Query<HeardQuery>,
) -> Response {
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if let Err(refusal) = check(bearer(presented), &st.expected) {
        return refusal.into_response();
    }
    let (Some(since), Some(until)) = (
        audiocore::instant::parse_utc(&q.since),
        audiocore::instant::parse_utc(&q.until),
    ) else {
        return (StatusCode::BAD_REQUEST, "unparseable window bound").into_response();
    };
    let root = st.root.clone();
    crate::route::json("sync heard", move || {
        crate::live_tier::heard(&root, since, until)
    })
    .await
}

#[derive(Deserialize)]
pub struct LiveTurnsIn {
    pub turns: Vec<crate::work::LiveTurn>,
}

/// How many were newly stored.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LiveStoredOut {
    pub stored: usize,
}

/// `POST /sync/live`: the live feed. Best effort: the archive pass transcribes
/// the minute again, so a dropped push loses no word.
pub async fn live_route(
    State(st): State<Arc<Gate>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if let Err(refusal) = check(bearer(presented), &st.expected) {
        return refusal.into_response();
    }
    let Ok(body) = serde_json::from_slice::<LiveTurnsIn>(&body) else {
        return (StatusCode::UNPROCESSABLE_ENTITY, "bad live turns").into_response();
    };
    let root = st.root.clone();
    match crate::route::blocking("sync live", move || {
        let mut conn = crate::work::open_write(&root)?;
        crate::work::ingest_live(&mut conn, &body.turns, chrono::Utc::now())
    })
    .await
    {
        Ok(stored) => axum::Json(LiveStoredOut { stored }).into_response(),
        Err(response) => response,
    }
}

/// `null` when nothing is enrolled.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct PromptOut {
    pub prompt: Option<String>,
}

pub fn routes(gate: Arc<Gate>) -> Router {
    Router::new()
        .route("/sync/capture", post(capture_route))
        .route(
            "/sync/vocabulary/prompt",
            axum::routing::get(vocabulary_prompt_route),
        )
        .route("/sync/live", post(live_route))
        .route("/sync/live/health", axum::routing::get(live_health_route))
        .route("/sync/heard", axum::routing::get(heard_route))
        .route(
            "/sync/record/health",
            axum::routing::get(record_health_route),
        )
        .with_state(gate)
}
