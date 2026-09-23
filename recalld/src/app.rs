//! Router assembly. The ingest plane has no DELETE route, by design
//! (docs/architecture.md, decision 2).
//!
//! Three planes, gated differently: ingest and work take their own tokens, the
//! sync plane its shared secret, and the browsing plane sits behind the
//! Nextcloud SSO gate. The last two are mounted only when configured.

use crate::tokens::Tokens;
use crate::{
    assign, audio, capture, conversations, devices, ingest, labels, labels_write, reads, reports,
    sessions, sources, spa, sync, upload, webauth, work,
};
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{delete, get, patch, post, put};
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
    /// The browsing plane's SSO gate. `None` = the browsing routes are not
    /// mounted at all.
    ///
    /// ⚠ Unlike the other credentials here, absent means unmounted, not open:
    /// these routes serve private transcripts.
    pub webauth: Option<webauth::GateState>,
    /// The Mac→fleet sync plane's shared secret. `None` = those routes are not
    /// mounted: they carry capture control and must never answer open.
    pub sync_token: Option<String>,
    /// The built Angular app. `None` = not served (the default, and what every
    /// test and dev run uses).
    pub frontend: Option<PathBuf>,
}

/// The browsing plane, behind the SSO gate.
fn browsing(st: webauth::GateState, root: PathBuf, log_path: PathBuf) -> Router {
    let capture_root = root.clone();
    let read = Arc::new(reads::State { root });
    Router::new()
        .route("/api/timeline", get(reads::timeline_route))
        // Device-exempt (`webauth::DEVICE_EXEMPT`): the mic apps poll this to
        // show who is recording, and carry no session.
        .route("/api/sources", get(sources::sources_route))
        .route("/api/search", get(reads::search_route))
        .route("/api/transcripts", get(reads::transcripts_route))
        .route("/api/review", get(reads::review_route))
        .route(
            "/api/conversations",
            get(conversations::conversations_route),
        )
        // Playback shares reads' state and its read-only connection: a clip is a
        // read of the meaning plane plus a read of the audio file.
        .route("/api/audio/{id}", get(audio::audio_route))
        .route("/api/audio-span", get(audio::audio_span_route))
        // Reads keep the read-only handle; writes take their own connection.
        .route(
            "/api/vocabulary",
            get(work::vocabulary_route).post(work::vocabulary_add_route),
        )
        .route(
            "/api/vocabulary/{id}",
            delete(work::vocabulary_delete_route),
        )
        .route("/api/speakers", get(labels::speakers_route))
        .route("/api/corrections", get(labels::corrections_route))
        .route(
            "/api/correction/{id}/audio",
            get(labels::correction_audio_route),
        )
        // The recorders' own status: heartbeats and upload outboxes. Their POSTs
        // are device-exempt in the gate (`webauth::DEVICE_EXEMPT`).
        .route(
            "/api/devices/heartbeat",
            get(devices::heartbeat_get_route).post(devices::heartbeat_post_route),
        )
        .route(
            "/api/devices/heartbeat/{device}",
            delete(devices::heartbeat_forget_route),
        )
        .route(
            "/api/devices/outbox",
            get(devices::outbox_get_route).post(devices::outbox_post_route),
        )
        .route(
            "/api/devices/outbox/{device}",
            delete(devices::outbox_forget_route),
        )
        .route("/api/correct", post(labels_write::correct_route))
        .route(
            "/api/turn/{id}/speaker",
            post(labels_write::turn_speaker_route),
        )
        .route(
            "/api/correction/{id}/speaker",
            post(labels_write::correction_reassign_route),
        )
        .route(
            "/api/correction/{id}/hide",
            post(labels_write::correction_hide_route),
        )
        .route("/api/sessions/{source}/assign", post(assign::assign_route))
        .route(
            "/api/sessions",
            get(sessions::sessions_route).post(upload::create_session_route),
        )
        .route(
            "/api/sessions/{source}",
            patch(sessions::rename_route).delete(sessions::delete_route),
        )
        .route(
            "/api/sessions/{source}/rediarize",
            post(sessions::rediarize_route),
        )
        .route(
            "/api/sessions/{source}/voice",
            post(sessions::name_voice_route),
        )
        .route(
            "/api/sessions/{source}/transcript",
            get(sessions::transcript_route),
        )
        .with_state(read)
        // The pause control. Its own state because it needs the gate config, to
        // record who pressed the button on routes that require no login.
        .merge(
            Router::new()
                .route("/api/capture", get(capture::status_route))
                .route("/api/capture/pause", post(capture::pause_route))
                .route("/api/capture/resume", post(capture::resume_route))
                .with_state(Arc::new(capture::Control {
                    root: capture_root,
                    webauth: Some(st.cfg.clone()),
                })),
        )
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
    let frontend = config.frontend.clone();
    let browsing_plane = config
        .webauth
        .clone()
        .map(|st| browsing(st, config.root.clone(), config.root.join("logs/client.log")));
    let sync_gate = config.sync_token.clone().map(|expected| {
        Arc::new(sync::Gate {
            expected,
            root: config.root.clone(),
        })
    });
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
        .with_state(config);
    let merged = match browsing_plane {
        Some(b) => base.merge(b),
        None => base,
    };
    let merged = match sync_gate {
        Some(gate) => merged.merge(sync::routes(gate)),
        None => merged,
    };
    // After every merge: a layer covers only the routes present when it is
    // applied, and a meeting upload is tens of MB on the browsing plane.
    let merged = merged.layer(DefaultBodyLimit::max(limit));
    // The shell answers only where nothing above matched, so a real route always
    // wins; an unmatched path under a server prefix is a 404 (`spa::serve`).
    match frontend {
        None => merged,
        Some(root) => {
            let fe = Arc::new(spa::Frontend { root });
            merged.fallback(move |req: axum::extract::Request| {
                spa::serve(axum::extract::State(fe.clone()), req.uri().clone())
            })
        }
    }
}
