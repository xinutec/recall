//! Router assembly: the surface is four routes, and the absence of a fifth is
//! load-bearing — there is no DELETE anywhere on this plane (docs/architecture.md,
//! decision 2).

use crate::ingest;
use crate::tokens::Tokens;
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
}

pub fn router(config: Arc<Config>) -> Router {
    let limit = config.max_body_bytes;
    Router::new()
        .route("/ingest/v1/health", get(ingest::health))
        .route("/ingest/v1/segments", get(ingest::list_segments))
        .route(
            "/ingest/v1/segments/{source}/{filename}",
            put(ingest::put_segment),
        )
        .route("/ingest/v1/blob/{source}/{filename}", get(ingest::get_blob))
        .route("/work/v1/lease", put(ingest::lease_job))
        .route("/work/v1/jobs/{id}/done", put(ingest::finish_job))
        .layer(DefaultBodyLimit::max(limit))
        .with_state(config)
}
