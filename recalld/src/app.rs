//! Router assembly. The absence of a DELETE anywhere on the ingest plane is
//! load-bearing (docs/architecture.md, decision 2).
//!
//! Two planes are assembled here and they are gated differently. The ingest and
//! work surfaces take their own tokens. The BROWSING surface — stage F1's port —
//! sits behind the Nextcloud SSO gate and is mounted only when that gate is
//! configured, so a dev or LAN-only recalld is unchanged.

use crate::tokens::Tokens;
use crate::{audio, ingest, labels, proxy, reads, reports, spa, webauth, work};
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{delete, get, post, put};
use std::path::PathBuf;
use std::sync::Arc;

/// A segment is ~60 s of mono FLAC — single-digit MB. The cap is generous
/// headroom over that, not a promise to accept arbitrary uploads.
pub const DEFAULT_MAX_BODY: usize = 64 * 1024 * 1024;

pub struct Config {
    /// The data root: blobs under `<root>/ingest/<source>/`, bookkeeping in
    /// `<root>/ingest.sqlite`.
    pub root: PathBuf,
    /// The write gate. `None` = open (dev, tests); the fleet mounts a file.
    pub tokens: Option<Tokens>,
    /// The read gate (listing, blobs). `None` = open, same pattern.
    pub read_token: Option<String>,
    pub max_body_bytes: usize,
    /// The browsing plane's SSO gate. `None` = the browsing routes are NOT
    /// mounted at all.
    ///
    /// ⚠ Absent means ABSENT, not open. Everywhere else in this repo an
    /// unconfigured credential means "inert, run open" — that is right for a
    /// LAN-only dev box and wrong here, because these routes serve household
    /// transcripts. An unconfigured recalld must not answer them at all rather
    /// than answer them to anyone.
    pub webauth: Option<webauth::GateState>,
    /// Where routes recalld does not serve yet are forwarded. `None` = a miss is
    /// a 404. See `proxy`: this is what lets Python be deleted one group at a
    /// time instead of all at once.
    pub upstream: Option<proxy::Upstream>,
    /// The built Angular app. `None` = not served (the default, and what every
    /// test and dev run uses).
    pub frontend: Option<PathBuf>,
}

/// The browsing plane: stage F1's ported routes, behind the SSO gate.
///
/// ⚠ **A cookie is scoped to a HOST, not a port** — which is what makes the
/// incremental cutover work in practice. recalld answers on `10.100.0.2:8001`
/// while the Python answers on `:8000`, and a browser sends the same
/// `recall_session` cookie to both. With the token format kept identical, a
/// person who signed in through the Python is already signed in here, so a route
/// group can move between the two without anyone signing in again.
fn browsing(st: webauth::GateState, root: PathBuf, log_path: PathBuf) -> Router {
    let read = Arc::new(reads::State { root });
    Router::new()
        .route("/api/timeline", get(reads::timeline_route))
        .route("/api/search", get(reads::search_route))
        .route("/api/transcripts", get(reads::transcripts_route))
        .route("/api/review", get(reads::review_route))
        // Playback shares reads' state and its read-only connection: a clip is a
        // read of the meaning plane plus a read of the audio file.
        .route("/api/audio/{id}", get(audio::audio_route))
        .route("/api/audio-span", get(audio::audio_span_route))
        // The first WRITE routes here. See `work`'s module note: reads keep the
        // read-only handle, writes take their own connection.
        .route(
            "/api/vocabulary",
            get(work::vocabulary_route).post(work::vocabulary_add_route),
        )
        .route(
            "/api/vocabulary/{id}",
            delete(work::vocabulary_delete_route),
        )
        .route("/api/refine", post(work::refine_route))
        // The labelling surface's READ half; its writes touch the system of
        // record and move in their own change (see `labels`).
        .route("/api/speakers", get(labels::speakers_route))
        .route("/api/corrections", get(labels::corrections_route))
        .route(
            "/api/correction/{id}/audio",
            get(labels::correction_audio_route),
        )
        .with_state(read)
        // Client reports carry their own state (a log path), not the database's.
        .merge(
            Router::new()
                .route("/api/log", post(reports::log_route))
                .with_state(Arc::new(reports::Reports { log_path })),
        )
        .route("/api/telemetry", post(reports::telemetry_route))
        .merge(webauth::routes(st.clone()))
        .layer(axum::middleware::from_fn_with_state(st, webauth::gate))
}

pub fn router(config: Arc<Config>) -> Router {
    let limit = config.max_body_bytes;
    let upstream = config.upstream.clone();
    let frontend = config.frontend.clone();
    let browsing_plane = config
        .webauth
        .clone()
        .map(|st| browsing(st, config.root.clone(), config.root.join("logs/client.log")));
    let base = Router::new()
        .route("/ingest/v1/health", get(ingest::health))
        .route("/ingest/v1/segments", get(ingest::list_segments))
        .route(
            "/ingest/v1/segments/{source}/{filename}",
            put(ingest::put_segment),
        )
        .route("/ingest/v1/liveness", get(ingest::liveness))
        .route("/ingest/v1/blob/{source}/{filename}", get(ingest::get_blob))
        .route("/work/v1/lease", put(ingest::lease_job))
        .route("/work/v1/jobs/{id}/done", put(ingest::finish_job))
        .layer(DefaultBodyLimit::max(limit))
        .with_state(config);
    let merged = match browsing_plane {
        Some(b) => base.merge(b),
        None => base,
    };
    // ⚠ Order matters and is the safety property: `fallback` runs ONLY where
    // nothing above matched, so a ported route always beats both the proxy and
    // the shell. A half-ported group can never silently keep serving the old
    // answer, and a typo'd path cannot shadow a real handler.
    //
    // Below that one rule decides between the two fallbacks: an `/api/*` miss is
    // Python's (it still owns those groups), anything else is the app shell's, so
    // a deep link like /sessions/meeting-x renders rather than 404ing.
    let frontend = frontend.map(|root| Arc::new(spa::Frontend { root }));
    match (upstream, frontend) {
        (None, None) => merged,
        (Some(up), None) => {
            merged.fallback(move |req: axum::extract::Request| proxy::forward(up.clone(), req))
        }
        (None, Some(fe)) => merged.fallback(move |uri: axum::http::Uri| {
            spa::serve(axum::extract::State(fe.clone()), uri)
        }),
        (Some(up), Some(fe)) => merged.fallback(move |req: axum::extract::Request| {
            let up = up.clone();
            let fe = fe.clone();
            async move {
                if req.uri().path().starts_with("/api/") {
                    proxy::forward(up, req).await
                } else {
                    spa::serve(axum::extract::State(fe), req.uri().clone()).await
                }
            }
        }),
    }
    // ⚠ The SPA is NOT mounted here. `recalld::spa` is ported and tested, but
    // recalld serves 2 of the ~28 /api/* routes the app calls, so serving the UI
    // from here would hand someone a half-working app — dev-lint's
    // DL-WIRE-ROUTE-DRIFT resolves this route table against the frontend's call
    // sites and said so, with 26 calls that would miss. Same rule that kept the
    // read routes off until webauth existed: do not expose a surface that is not
    // ready. This gains a `frontend` field when the route groups are done.
}
