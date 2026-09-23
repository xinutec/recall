//! The Mac→fleet sync plane: the routes the one-way peer dials in on.
//!
//! Every exchange is Mac-initiated: the Mac is a one-way `WireGuard` peer that
//! may dial the fleet, and nothing may dial back. A pause pressed in the web UI
//! therefore reaches the microphone by the Mac polling for it, which is why
//! [`capture_route`] long-polls.
//!
//! The credential is `RECALL_SYNC_TOKEN`, a shared secret on both ends. It
//! grants nothing on the browsing plane.
//!
//! Routes: the capture handshake (audiod's mirror), the vocabulary prompt (the
//! runner), the instant feed (recall-live), and the live tier's numbers and the
//! microphones' speech (both for the doctor).

use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use subtle::ConstantTimeEq;

/// The secret the Mac presents, and the data it is presented for.
///
/// No "unconfigured" variant: without a token there is no [`routes`] call and
/// no mounted route, so the plane cannot be answered open by accident.
pub struct Gate {
    pub expected: String,
    pub root: std::path::PathBuf,
}

const BEARER: &str = "Bearer ";

/// The token out of an `Authorization: Bearer <token>` header.
#[must_use]
pub fn bearer(header: Option<&str>) -> Option<&str> {
    header?.strip_prefix(BEARER)
}

/// Authorise a sync request, or the status and detail to answer with instead.
///
/// Constant-time compare, so timing does not reveal how much of a guess was
/// right. Returns the parts rather than a built `Response`, so the error stays
/// small and a test can assert on the status.
pub fn check(presented: Option<&str>, expected: &str) -> Result<(), (StatusCode, &'static str)> {
    let ok = presented.is_some_and(|p| {
        // `ct_eq` short-circuits on unequal length; the length of a
        // fixed-format token is not secret.
        p.as_bytes().ct_eq(expected.as_bytes()).into()
    });
    if ok {
        Ok(())
    } else {
        Err((StatusCode::UNAUTHORIZED, "bad sync token"))
    }
}

/// What the Mac reports it currently has applied, each mirror pass.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Applied {
    pub running: bool,
    /// The ISO resume-by it applied, or absent while recording.
    #[serde(default)]
    pub paused_until: Option<String>,
    /// Each source's last-proved-recording time. Defaulted, so a Mac that omits
    /// it reports no liveness rather than failing the exchange.
    #[serde(default)]
    pub source_liveness: serde_json::Map<String, serde_json::Value>,
    /// Seconds to hang while the intent still equals `known_intent`. Defaults to
    /// zero: answer at once.
    #[serde(default)]
    pub wait: f64,
    /// The intent the Mac has already applied; `None` means running.
    #[serde(default)]
    pub known_intent: Option<String>,
}

/// The fleet's desired capture state, for the Mac to mirror onto its pause file.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IntentOut {
    pub paused_until: Option<String>,
}

/// Never hold the exchange past this (as `GET /api/capture`): proxies and
/// thread pools need a horizon.
const WAIT_CAP: std::time::Duration = std::time::Duration::from_secs(25);
/// Re-derive the intent this often while hanging, so a pause elapsing (which
/// has no writer to notify) surfaces within one slice.
const WAIT_SLICE: std::time::Duration = std::time::Duration::from_secs(2);

/// `source_liveness` carries instants as strings and nothing else; the reader
/// would drop anything more.
fn all_strings(map: &serde_json::Map<String, serde_json::Value>) -> bool {
    map.values().all(serde_json::Value::is_string)
}

/// `POST /sync/capture`: the capture-control handshake in one round trip. The
/// Mac reports what it applied and reads back what the fleet wants.
///
/// ⚠ The report is recorded once, before the hang, which re-reads only the
/// intent. Recording inside the loop would keep a Mac that died mid-hang looking
/// alive for the whole cap.
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
    // ⚠ Subscribe before each derive, so a press landing between a derive and
    // the following wait is not a lost wakeup.
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
        // Notify for the fast path, slice as the floor (as in
        // `capture::status_route`): an elapsing pause has no writer, and a CLI
        // pause writes from another process, so the timeout still re-derives.
        // The notify removes the up-to-a-slice delay on a press.
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

/// Authorise, then answer a read from the meaning database off the request
/// thread.
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

/// `GET /sync/vocabulary/prompt`: the glossary as an ASR prompt. The runner
/// reads it and hands it to the model shim, which does no I/O beyond its stdio
/// and the audio path it is given.
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

/// The windows the caller wants the live tier measured over.
///
/// No defaults: the doctor owns every window.
#[derive(Deserialize)]
pub struct LiveHealthQuery {
    pub lag_since: String,
    pub window_since: String,
    pub window_until: String,
}

/// `GET /sync/live/health`: how the instant feed is running, for the doctor.
/// On the sync plane because the reader is the Mac, which holds this token.
/// Numbers only; the verdicts stay on the Mac.
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

/// The window `GET /sync/heard` measures.
#[derive(Deserialize)]
pub struct HeardQuery {
    pub since: String,
    pub until: String,
}

/// `GET /sync/heard`: per device source, the audio delivered and the speech in
/// it, for the doctor's deaf-microphone check. Numbers only, like
/// [`live_health_route`].
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

/// A batch of provisional live turns from the Mac.
#[derive(Deserialize)]
pub struct LiveTurnsIn {
    pub turns: Vec<crate::work::LiveTurn>,
}

/// How many were newly stored; turns already present are not counted.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LiveStoredOut {
    pub stored: usize,
}

/// `POST /sync/live`: the instant feed.
///
/// Best-effort: the archive pass transcribes the same minute again, so a
/// dropped live push delays the feed and never loses a word.
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

/// The glossary prompt's wire shape. `null` when nothing is enrolled.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct PromptOut {
    pub prompt: Option<String>,
}

/// The sync plane's routes. Mounted only where a token is configured.
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
        .with_state(gate)
}
