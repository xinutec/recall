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
use axum::extract::{Query, State};
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

/// How many jobs the Mac asks for. Defaulted so a client that omits it gets what
/// the Python gave it.
#[derive(Deserialize)]
pub struct JobsQuery {
    #[serde(default = "default_job_limit")]
    pub limit: i64,
}

const fn default_job_limit() -> i64 {
    50
}

/// `GET /sync/jobs` — what the fleet wants the Mac to do.
pub async fn jobs_route(
    State(st): State<Arc<Gate>>,
    headers: axum::http::HeaderMap,
    Query(q): Query<JobsQuery>,
) -> Response {
    let limit = q.limit;
    gated_read(&st, &headers, "sync jobs", move |conn| {
        crate::work::pending_jobs(conn, limit)
    })
    .await
}

/// Which id space `job_id` belongs to. Defaulted to `refine`, so a Mac too old
/// to send it acknowledges refines exactly as it always did.
#[derive(Deserialize)]
pub struct DoneQuery {
    #[serde(default = "default_job_type")]
    pub r#type: String,
}

fn default_job_type() -> String {
    "refine".to_owned()
}

/// `POST /sync/jobs/{id}/done` — the Mac has taken the job.
///
/// ⚠ "Done" means DIFFERENT things per type and neither is "transcribed". For a
/// refine it is processed; for an upload it means the Mac now HOLDS the audio and
/// will ASR it, so the row stops being served. Conflating them would either
/// re-serve work already taken or retire work never done.
pub async fn job_done_route(
    State(st): State<Arc<Gate>>,
    axum::extract::Path(job_id): axum::extract::Path<i64>,
    Query(q): Query<DoneQuery>,
    headers: axum::http::HeaderMap,
) -> Response {
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if let Err(refusal) = check(bearer(presented), &st.expected) {
        return refusal.into_response();
    }
    let kind = q.r#type;
    if kind != "refine" && kind != "upload" {
        return (
            StatusCode::BAD_REQUEST,
            format!("unknown job type {kind:?}"),
        )
            .into_response();
    }
    let root = st.root.clone();
    match crate::route::blocking("sync job done", move || {
        let conn = crate::work::open_write(&root)?;
        if kind == "upload" {
            crate::work::mark_transcribed(&conn, job_id)
        } else {
            crate::work::mark_refine_done(&conn, job_id, chrono::Utc::now())
        }
    })
    .await
    {
        Ok(()) => crate::route::ack(),
        Err(response) => response,
    }
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
/// ⚠ Best-effort by design, and that is what makes it safe to be lossy: the
/// archive segment push carries these turns again regardless, so a dropped live
/// push only delays the instant feed and never loses a word.
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
        crate::work::ingest_live(&mut conn, &body.turns)
    })
    .await
    {
        Ok(stored) => axum::Json(LiveStoredOut { stored }).into_response(),
        Err(response) => response,
    }
}

/// A catch-up's worth of segments in one request — the same items the single
/// push takes. Blobs still travel separately.
#[derive(Deserialize)]
pub struct SegmentBatchIn {
    pub segments: Vec<crate::work::SegmentIn>,
}

/// One result per pushed segment, ALIGNED BY INDEX with the request.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct SegmentsStoredOut {
    pub results: Vec<crate::work::SegmentStoredOut>,
}

/// `POST /sync/segments` — the Mac's transcripts reach the fleet here.
pub async fn segments_route(
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
    let Ok(seg) = serde_json::from_slice::<crate::work::SegmentIn>(&body) else {
        return (StatusCode::UNPROCESSABLE_ENTITY, "bad segment").into_response();
    };
    let root = st.root.clone();
    match crate::route::blocking("sync segments", move || {
        let mut conn = crate::work::open_write(&root)?;
        crate::work::ingest_segment(&mut conn, &seg, &root)
    })
    .await
    {
        Ok(out) => axum::Json(out).into_response(),
        Err(response) => response,
    }
}

/// `POST /sync/segments/batch` — the same, many at a time (#1346).
///
/// ⚠ **Items are processed SEQUENTIALLY and a failure aborts the rest**, which
/// preserves the semantics the Mac's pusher is built on: it marks each id pushed
/// only after the chunk is acknowledged, and a transport failure must abort the
/// pass BEFORE the watermark advances so the failed segments retry next cycle.
/// Answering partial success would advance the watermark past segments that never
/// landed.
pub async fn segments_batch_route(
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
    let Ok(batch) = serde_json::from_slice::<SegmentBatchIn>(&body) else {
        return (StatusCode::UNPROCESSABLE_ENTITY, "bad segment batch").into_response();
    };
    let root = st.root.clone();
    match crate::route::blocking("sync segments batch", move || {
        let mut conn = crate::work::open_write(&root)?;
        let mut results = Vec::with_capacity(batch.segments.len());
        for seg in &batch.segments {
            results.push(crate::work::ingest_segment(&mut conn, seg, &root)?);
        }
        Ok(SegmentsStoredOut { results })
    })
    .await
    {
        Ok(out) => axum::Json(out).into_response(),
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
        .route("/sync/jobs", axum::routing::get(jobs_route))
        .route("/sync/jobs/{job_id}/done", post(job_done_route))
        .route("/sync/live", post(live_route))
        .route(
            "/sync/audio",
            axum::routing::get(audio_present_route).post(audio_push_route),
        )
        .route("/sync/audio/file", axum::routing::get(audio_file_route))
        .route("/sync/segments", post(segments_route))
        .route("/sync/segments/batch", post(segments_batch_route))
        // ⚠ WITHOUT THIS THE AUDIO PUSH IS DEAD ON ARRIVAL. `app::router` layers
        // its body limit onto the ingest router only, and a `.layer` applies to
        // routes added BEFORE it — so this router, merged afterwards, would
        // inherit axum's 2 MB default and refuse every real segment, let alone a
        // 62 MB meeting. The uvicorn it replaces had no cap at all.
        //
        // Generous rather than tight because the upload STREAMS to a temp file:
        // the cost of a big push is disk, which the archive volume already holds,
        // not memory. A cap that merely looks prudent would reject recordings the
        // Mac then retries for ever.
        .layer(axum::extract::DefaultBodyLimit::max(1024 * 1024 * 1024))
        .with_state(gate)
}

// --- the audio blob plane -----------------------------------------------------

/// A single path component the fleet will trust as a directory or file name.
///
/// ⚠ **This is deliberately NOT `audiocore::names::parse`, and substituting it
/// would break the meeting sync silently.** That grammar accepts only
/// `flac|opus|ogg|wav`, and every uploaded meeting's audio is `.mp3` — so the
/// stricter check would refuse every one of them with a 400 the Mac would retry
/// for ever. This is a path-traversal guard, not a filename schema: the Mac is
/// authenticated, but a compromised token must not become arbitrary file write.
///
/// Rejects exactly what the Python rejects: empty, either separator, `..`
/// anywhere, and a leading dot (which would let a push land as a hidden file).
#[must_use]
pub fn safe_component(component: &str) -> Option<&str> {
    if component.is_empty()
        || component.contains('/')
        || component.contains('\\')
        || component.contains("..")
        || component.starts_with('.')
    {
        return None;
    }
    Some(component)
}

/// Resolve `<root>/<source>/<name>`, or `None` if either component is unsafe.
fn blob_path(root: &std::path::Path, source: &str, name: &str) -> Option<std::path::PathBuf> {
    Some(
        root.join(safe_component(source)?)
            .join(safe_component(name)?),
    )
}

#[derive(Deserialize)]
pub struct BlobQuery {
    pub source: String,
    pub name: String,
}

/// Whether the fleet already holds this blob — lets the Mac skip re-sending the
/// (immutable) bytes on every sync pass.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct AudioPresentOut {
    pub present: bool,
}

/// Whether the fleet NEWLY stored it. `false` = it already had it.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct AudioStoredOut {
    pub stored: bool,
}

/// `GET /sync/audio` — presence.
pub async fn audio_present_route(
    State(st): State<Arc<Gate>>,
    headers: axum::http::HeaderMap,
    Query(q): Query<BlobQuery>,
) -> Response {
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if let Err(refusal) = check(bearer(presented), &st.expected) {
        return refusal.into_response();
    }
    let Some(path) = blob_path(&st.root, &q.source, &q.name) else {
        return (StatusCode::BAD_REQUEST, "unsafe path component").into_response();
    };
    axum::Json(AudioPresentOut {
        present: path.is_file(),
    })
    .into_response()
}

/// `GET /sync/audio/file` — fetch the bytes.
///
/// ⚠ Streams from disk rather than reading the file into memory: an uploaded
/// meeting can be hours long, and the Mac fetches these to transcribe them.
pub async fn audio_file_route(
    State(st): State<Arc<Gate>>,
    headers: axum::http::HeaderMap,
    Query(q): Query<BlobQuery>,
) -> Response {
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if let Err(refusal) = check(bearer(presented), &st.expected) {
        return refusal.into_response();
    }
    let Some(path) = blob_path(&st.root, &q.source, &q.name) else {
        return (StatusCode::BAD_REQUEST, "unsafe path component").into_response();
    };
    // Read on the blocking pool, the same way `/ingest/v1/blob` serves bytes.
    // ⚠ The whole file lands in memory, so it is bounded by what the archive
    // holds: the largest meeting is 62 MB against this pod's 1 GiB. That is fine
    // for the Mac fetching one at a time to transcribe, and would not be if this
    // ever served many concurrent readers.
    match tokio::task::spawn_blocking(move || std::fs::read(&path)).await {
        Ok(Ok(bytes)) => (StatusCode::OK, bytes).into_response(),
        Ok(Err(_)) => (StatusCode::NOT_FOUND, "no such audio").into_response(),
        Err(err) => crate::route::faulted("audio file", &err),
    }
}

/// `POST /sync/audio` — push a blob.
///
/// ⚠ **The archive is immutable: same path, same content.** An existing file is
/// never overwritten, which is what makes the push idempotent and safe for the
/// Mac to retry after a timeout it cannot tell from a failure.
///
/// ⚠ Written through a temp file and renamed, unlike the Python's direct copy.
/// A push interrupted midway would otherwise leave PARTIAL BYTES under the final
/// name — and because the presence check is `exists()`, the Mac would then be
/// told the fleet holds a file that is truncated, for ever.
pub async fn audio_push_route(
    State(st): State<Arc<Gate>>,
    headers: axum::http::HeaderMap,
    mut form: axum::extract::Multipart,
) -> Response {
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if let Err(refusal) = check(bearer(presented), &st.expected) {
        return refusal.into_response();
    }
    let (mut source, mut name, mut staged) = (None, None, None);
    while let Ok(Some(mut field)) = form.next_field().await {
        match field.name().map(ToOwned::to_owned).as_deref() {
            Some("source") => source = field.text().await.ok(),
            Some("name") => name = field.text().await.ok(),
            Some("file") => {
                // ⚠ STREAMED to a temp file, never buffered. The largest meeting
                // in the archive is 62 MB and this pod is capped at 1 GiB, so
                // `field.bytes()` would put a whole recording in memory — and
                // would also inherit a body cap the Python never had, refusing
                // anything past it with a 4xx the Mac retries for ever.
                let mut tmp = match tempfile::NamedTempFile::new_in(&st.root) {
                    Ok(tmp) => tmp,
                    Err(err) => return crate::route::faulted("sync audio push", &err),
                };
                loop {
                    match field.chunk().await {
                        Ok(Some(chunk)) => {
                            if let Err(err) = std::io::Write::write_all(&mut tmp, &chunk) {
                                return crate::route::faulted("sync audio push", &err);
                            }
                        }
                        Ok(None) => break,
                        Err(err) => {
                            return (StatusCode::BAD_REQUEST, format!("truncated upload: {err}"))
                                .into_response();
                        }
                    }
                }
                staged = Some(tmp);
            }
            _ => {}
        }
    }
    let (Some(source), Some(name), Some(staged)) = (source, name, staged) else {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            "source, name and file are required",
        )
            .into_response();
    };
    let Some(dest) = blob_path(&st.root, &source, &name) else {
        return (StatusCode::BAD_REQUEST, "unsafe path component").into_response();
    };
    match crate::route::blocking("sync audio push", move || Ok(store_blob(&dest, staged))).await {
        Ok(Ok(stored)) => axum::Json(AudioStoredOut { stored }).into_response(),
        Ok(Err(err)) => crate::route::faulted("sync audio push", &err),
        Err(response) => response,
    }
}

/// Move the staged upload into place unless the blob is already held.
/// `Ok(false)` = already held, which is the idempotent case, not a failure.
fn store_blob(dest: &std::path::Path, tmp: tempfile::NamedTempFile) -> std::io::Result<bool> {
    if dest.exists() {
        return Ok(false);
    }
    let Some(dir) = dest.parent() else {
        return Err(std::io::Error::other("blob path has no directory"));
    };
    std::fs::create_dir_all(dir)?;
    tmp.as_file().sync_all()?;
    match tmp.persist_noclobber(dest) {
        Ok(_) => {}
        // Lost the race with another push of the same immutable blob: the file
        // is there, which is all the caller asked about.
        Err(err) if err.error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(err) => return Err(err.error),
    }
    std::fs::File::open(dir)?.sync_all()?;
    Ok(true)
}
