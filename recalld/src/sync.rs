//! The Mac→fleet sync plane: the routes the one-way peer dials in on.
//!
//! ⚠ **Every exchange here is Mac-INITIATED, and that inversion is a security
//! property, not a style.** The Mac is a one-way `WireGuard` peer: it may dial the
//! fleet and nothing may dial back. So a pause pressed on Isis's web UI cannot be
//! pushed to the microphone — the Mac polls for it. That is why a control route
//! lives on a *sync* plane at all, and why it hangs (see [`capture_route`])
//! instead of answering at once.
//!
//! ⚠ **The credential is a shared secret, not a login.** One peer, one token, and
//! `RECALL_SYNC_TOKEN` is the same string on both ends. It is NOT the browsing
//! plane's cookie and grants none of it — these routes are exempt from the SSO
//! gate for the same reason the recording plane is: the caller is a daemon and
//! carries no session.

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use subtle::ConstantTimeEq;

/// The secret the Mac presents, and the data it is presented for.
///
/// ⚠ There is no "unconfigured" variant on purpose. Python guards the same
/// routes with a runtime 503 for a token that is missing, which is unreachable
/// there because the routes are only registered once one is set. Here the same
/// rule is structural: no token, no [`routes`] call, no mounted route — so
/// `/sync/*` stays with the Python upstream and cannot be answered open by
/// accident.
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

/// The values `source_liveness` is allowed to carry. Python's model declares
/// `dict[str, str]` and pydantic rejects anything else; accepting more here
/// would be a silent widening, and the reader would drop the extra anyway.
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
        // ⚠ Sleep rather than park on a notify, for the reason
        // `capture::status_route` gives at length: a pause reaching its deadline
        // has no writer to signal at all, and a break-glass CLI pause writes the
        // row from another process entirely.
        //
        // The cost is real and worth naming: the Python this replaces parks on
        // an in-process notify, so a press reached the Mac in ~RTT and here it
        // takes up to one slice. That is the same trade `GET /api/capture`
        // already ships to every phone in the house, so this is consistency
        // rather than a new regression — but a notify ON TOP of the slice would
        // buy back both, and is worth doing for both routes at once.
        tokio::time::sleep(WAIT_SLICE.min(deadline - now)).await;
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
/// Every read route on this plane is the same three steps — check the bearer,
/// open the store off the request thread, serialise — and writing them out per
/// route is four places for one of them to drift.
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

/// `GET /sync/labels` — the human voice-namings, fleet→Mac.
///
/// ⚠ This is the Mac's ONLY path to the names. The UI lives on the fleet, so a
/// person naming a voice there reaches the master archive and the voiceprint
/// enrolment through here or not at all.
pub async fn labels_route(State(st): State<Arc<Gate>>, headers: axum::http::HeaderMap) -> Response {
    gated_read(&st, &headers, "sync labels", crate::labels::cluster_namings).await
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

/// `GET /sync/devices/heartbeats` — when each mic app last said it was running.
///
/// ⚠ On the SYNC plane because the READER is the Mac. The apps' own
/// `POST /api/devices/heartbeat` stays unauthenticated, which is the credential
/// THEY have; this side carries the one the Mac already holds. Neither gains
/// anything it did not need.
pub async fn heartbeats_route(
    State(st): State<Arc<Gate>>,
    headers: axum::http::HeaderMap,
) -> Response {
    gated_read(&st, &headers, "sync heartbeats", crate::devices::beats_out).await
}

/// `GET /sync/devices/outbox` — what each phone last said it was still holding.
pub async fn outbox_route(State(st): State<Arc<Gate>>, headers: axum::http::HeaderMap) -> Response {
    gated_read(&st, &headers, "sync outboxes", crate::devices::reports_out).await
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
        .route("/sync/labels", axum::routing::get(labels_route))
        .route(
            "/sync/vocabulary/prompt",
            axum::routing::get(vocabulary_prompt_route),
        )
        .route(
            "/sync/devices/heartbeats",
            axum::routing::get(heartbeats_route),
        )
        .route("/sync/devices/outbox", axum::routing::get(outbox_route))
        .with_state(gate)
}
