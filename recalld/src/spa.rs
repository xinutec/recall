//! Serving the built Angular app from the same origin as its API.
//!
//! Three rules:
//!
//! 1. A path under a server prefix that reached the fallback is a 404, never
//!    `index.html`. The shell with a 200 turns "no such route" into HTML a
//!    client then fails to parse as JSON, far from the cause.
//! 2. `index.html` is `no-cache`; hashed bundles are immutable. The shell names
//!    the current bundles, so a cached one hides a deploy until a hard refresh.
//! 3. A request may not escape the frontend directory: `../` is resolved and
//!    checked against the root.

use axum::body::Body;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Where the built app lives.
pub struct Frontend {
    pub root: PathBuf,
}

/// Prefixes the server answers itself. A miss under one of these is a miss.
pub const SERVER_PREFIXES: &[&str] = &["/api/", "/sync/", "/ingest/", "/work/"];

/// Resolve a request path to a file INSIDE the frontend root, or None.
///
/// ⚠ The containment check is on the canonicalised path, so `..` segments and
/// symlinks are resolved first; the raw string would pass `a/../../etc/passwd`.
#[must_use]
pub fn resolve(root: &Path, request_path: &str) -> Option<PathBuf> {
    let root = root.canonicalize().ok()?;
    let candidate = root
        .join(request_path.trim_start_matches('/'))
        .canonicalize()
        .ok()?;
    if candidate != root && !candidate.starts_with(&root) {
        return None;
    }
    candidate.is_file().then_some(candidate)
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("woff2") => "font/woff2",
        Some("ico") => "image/x-icon",
        Some("webmanifest") => "application/manifest+json",
        _ => "application/octet-stream",
    }
}

fn file(path: &Path, cache: &str) -> Response {
    match std::fs::read(path) {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, content_type(path)),
                (header::CACHE_CONTROL, cache),
            ],
            Body::from(bytes),
        )
            .into_response(),
        Err(e) => {
            tracing::warn!("frontend read failed for {}: {e}", path.display());
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// The SPA fallback. Mounted last, so every explicit route wins.
///
/// `async` with no await: axum requires the handler signature, and reading a
/// built asset off local disk is not worth a blocking-pool hop.
#[allow(clippy::unused_async)]
pub async fn serve(State(fe): State<Arc<Frontend>>, uri: axum::http::Uri) -> Response {
    let path = uri.path();
    // Rule 1.
    if SERVER_PREFIXES.iter().any(|p| path.starts_with(p)) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    if let Some(asset) = resolve(&fe.root, path) {
        // Rule 2a: content-hashed bundles are immutable.
        return file(&asset, "public, max-age=31536000, immutable");
    }
    match resolve(&fe.root, "index.html") {
        // Rule 2b: the shell names the current bundles and must never be cached.
        Some(index) => file(&index, "no-cache"),
        None => (StatusCode::NOT_FOUND, "frontend not built").into_response(),
    }
}
