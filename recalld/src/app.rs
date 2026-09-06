//! Router assembly. The absence of a DELETE anywhere on the ingest plane is
//! load-bearing (docs/architecture.md, decision 2).
//!
//! Two planes are assembled here and they are gated differently. The ingest and
//! work surfaces take their own tokens. The BROWSING surface — stage F1's port —
//! sits behind the Nextcloud SSO gate and is mounted only when that gate is
//! configured, so a dev or LAN-only recalld is unchanged.

use crate::tokens::Tokens;
use crate::{audio, ingest, reads, webauth};
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, put};
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
}

/// The browsing plane: stage F1's ported routes, behind the SSO gate.
///
/// ⚠ **A cookie is scoped to a HOST, not a port** — which is what makes the
/// incremental cutover work in practice. recalld answers on `10.100.0.2:8001`
/// while the Python answers on `:8000`, and a browser sends the same
/// `recall_session` cookie to both. With the token format kept identical, a
/// person who signed in through the Python is already signed in here, so a route
/// group can move between the two without anyone signing in again.
fn browsing(st: webauth::GateState, root: PathBuf) -> Router {
    let read = Arc::new(reads::State { root });
    Router::new()
        .route("/api/timeline", get(reads::timeline_route))
        .route("/api/search", get(reads::search_route))
        // Playback shares reads' state and its read-only connection: a clip is a
        // read of the meaning plane plus a read of the audio file.
        .route("/api/audio/{id}", get(audio::audio_route))
        .route("/api/audio-span", get(audio::audio_span_route))
        .with_state(read)
        .merge(webauth::routes(st.clone()))
        .layer(axum::middleware::from_fn_with_state(st, webauth::gate))
}

pub fn router(config: Arc<Config>) -> Router {
    let limit = config.max_body_bytes;
    let browsing_plane = config
        .webauth
        .clone()
        .map(|st| browsing(st, config.root.clone()));
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
    match browsing_plane {
        Some(b) => base.merge(b),
        None => base,
    }
    // ⚠ The SPA is NOT mounted here. `recalld::spa` is ported and tested, but
    // recalld serves 2 of the ~28 /api/* routes the app calls, so serving the UI
    // from here would hand someone a half-working app — dev-lint's
    // DL-WIRE-ROUTE-DRIFT resolves this route table against the frontend's call
    // sites and said so, with 26 calls that would miss. Same rule that kept the
    // read routes off until webauth existed: do not expose a surface that is not
    // ready. This gains a `frontend` field when the route groups are done.
}
