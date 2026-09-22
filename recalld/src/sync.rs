//! The Mac→fleet sync plane: the routes the one-way peer dials in on.
//!
//! Every exchange here is Mac-initiated, and that inversion is a security
//! property: the Mac is a one-way `WireGuard` peer that may dial the fleet, and
//! nothing may dial back. A pause pressed in the web UI therefore reaches the
//! microphone by the Mac polling for it, which is why a control route lives on
//! a sync plane at all and why it hangs ([`capture_route`]) instead of answering
//! at once.
//!
//! The credential is a shared secret, not a login: `RECALL_SYNC_TOKEN` is the
//! same string on both ends. It is not the browsing plane's cookie and grants
//! none of it; the caller is a daemon and carries no session.
//!
//! Four routes, four callers: the capture handshake (audiod's mirror), the
//! vocabulary prompt (the runner), the instant feed (recall-live) and the live
//! tier's numbers (the doctor).

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
/// ⚠ Constant-time compare. A byte-by-byte one leaks how much of a guess was
/// right through its timing, which turns a 256-bit secret into a few hundred
/// requests per byte.
///
/// Returns the parts rather than a built `Response` so the error stays small
/// enough to return by value, and so a test can assert on the status without
/// unpicking a body.
pub fn check(presented: Option<&str>, expected: &str) -> Result<(), (StatusCode, &'static str)> {
    let ok = presented.is_some_and(|p| {
        // ⚠ `ct_eq` is constant-time in the CONTENTS, not the length — an
        // unequal length short-circuits. That is the same leak
        // `hmac.compare_digest` accepts, and it is fine: the length of a
        // fixed-format token is not the secret.
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
    /// Each source's last-proved-recording time. Defaulted, so a Mac too old to
    /// send it reports no liveness rather than failing the exchange.
    #[serde(default)]
    pub source_liveness: serde_json::Map<String, serde_json::Value>,
    /// Seconds to hang while the intent still equals `known_intent`. Defaulted,
    /// so an older Mac short-polls exactly as it always did.
    #[serde(default)]
    pub wait: f64,
    /// The intent the Mac has already applied — `None` meaning "running".
    #[serde(default)]
    pub known_intent: Option<String>,
}

/// The fleet's desired capture state, for the Mac to mirror onto its pause file.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IntentOut {
    pub paused_until: Option<String>,
}

/// Never hold the exchange past this. Matches `GET /api/capture`'s cap, and for
/// the same reason: proxies and thread pools need a horizon.
const WAIT_CAP: std::time::Duration = std::time::Duration::from_secs(25);
/// Re-derive the intent this often while hanging, so a pause ELAPSING — which
/// has no writer at all, and so can never notify — surfaces within one slice.
const WAIT_SLICE: std::time::Duration = std::time::Duration::from_secs(2);

/// `source_liveness` carries instants as strings and nothing else; the reader
/// would drop anything more.
fn all_strings(map: &serde_json::Map<String, serde_json::Value>) -> bool {
    map.values().all(serde_json::Value::is_string)
}

/// `POST /sync/capture` — the capture-control handshake, in one round trip: the
/// Mac reports what it applied, and reads back what the fleet wants.
///
/// ⚠ The report is recorded BEFORE the hang, and the hang re-reads only the
/// intent. Recording inside the loop would re-stamp the freshness clock every
/// slice and make a Mac that died mid-hang keep looking alive for the whole cap.
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
    // ⚠ SUBSCRIBE BEFORE THE FIRST DERIVE, and re-subscribe before each later
    // one: a press landing between a derive and the wait that follows it is the
    // lost wakeup, and holding a receiver across both is what closes it.
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
        // Notify for the fast path, slice as the floor — the same shape
        // `capture::status_route` uses, and for the reason it gives at length: a
        // pause reaching its deadline has NO writer to signal, and a break-glass
        // CLI pause writes the row from another process entirely. So the timeout
        // still has to re-derive; what the notify removes is the up-to-a-slice
        // delay on a press, which is the household's privacy control.
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

/// Authorise, then answer a read from the meaning database.
///
/// Check the bearer, open the store off the request thread, serialise.
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

/// `GET /sync/vocabulary/prompt` — the household glossary as an ASR prompt.
///
/// ⚠ Read at RUNNER STARTUP, never fetched by the model shim itself: a shim does
/// no I/O beyond its stdio and the audio path it is handed, so the prompt is
/// carried to it. Putting a database handle inside the model process would couple
/// the worker back to state it is meant to have given up.
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
/// ⚠ **No defaults.** The doctor owns every threshold and every window, and a
/// default here would be a second opinion that only shows up when a caller
/// forgets one — which is the reading nobody checks.
#[derive(Deserialize)]
pub struct LiveHealthQuery {
    pub lag_since: String,
    pub window_since: String,
    pub window_until: String,
}

/// `GET /sync/live/health` — how the instant feed is running, for the doctor.
///
/// ⚠ On the SYNC plane because the READER is the Mac, and it already holds this
/// token. ⚠ Numbers only: the verdicts stay on the Mac so fleetwatch sees one
/// grader (#1671).
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
    // ⚠ Parsed HERE, so an unspellable bound is the caller's 400 rather than a
    // TEXT comparison against something that only looks like a timestamp.
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

/// A batch of provisional live turns from the Mac.
#[derive(Deserialize)]
pub struct LiveTurnsIn {
    pub turns: Vec<crate::work::LiveTurn>,
}

/// How many were NEWLY stored — present ones are skipped, not counted.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LiveStoredOut {
    pub stored: usize,
}

/// `POST /sync/live` — the instant feed.
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

/// The sync plane's routes. Mounted only where a token is configured — see
/// [`Gate`].
pub fn routes(gate: Arc<Gate>) -> Router {
    Router::new()
        .route("/sync/capture", post(capture_route))
        .route(
            "/sync/vocabulary/prompt",
            axum::routing::get(vocabulary_prompt_route),
        )
        .route("/sync/live", post(live_route))
        .route("/sync/live/health", axum::routing::get(live_health_route))
        .with_state(gate)
}
