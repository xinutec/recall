//! Serving the built Angular app (stage F1), ported from `recall.api`'s `spa`.
//!
//! ⚠ **Built and tested, and deliberately NOT mounted yet** — the same rule that
//! kept the read routes off the router until webauth existed. recalld serves 2 of
//! the ~28 `/api/*` routes the SPA calls, so serving the app from here would hand
//! someone a half-working UI whose other calls 404. dev-lint's
//! `DL-WIRE-ROUTE-DRIFT` says so precisely: it resolves the axum route table
//! against the frontend's call sites, and mounting this made it report 26 calls
//! that would miss. That is a finding, not noise, so it is being obeyed rather
//! than waived. Mount this when the route groups are done; `app::router` gains a
//! `frontend` field then, not before.
//!
//! One origin: the SPA and its API answer on the same host, so there is no CORS
//! and no second deployment. Three rules, each of which was learned rather than
//! designed, and none of which a generic static-file handler would give:
//!
//! 1. ⚠ **`/api/*` that falls through here is a 404, never `index.html`.** A miss
//!    on the API must look like a miss. Returning the SPA shell with status 200
//!    turns "this route does not exist" into "here is some HTML", which a client
//!    then tries to parse as JSON — the error becomes a parse failure a long way
//!    from its cause.
//! 2. ⚠ **`index.html` is `no-cache`; hashed assets are immutable.** index.html
//!    names the current bundles, so caching it means a deploy is not picked up
//!    until a hard refresh — the bug that served stale code from isis. The
//!    bundles themselves carry a content hash in the name, so they can be cached
//!    for a year safely.
//! 3. ⚠ **A request may not escape the frontend directory.** `../` is resolved
//!    and checked against the root, so a crafted path cannot read the archive,
//!    the database, or the token file.

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

/// Resolve a request path to a file INSIDE the frontend root, or None.
///
/// ⚠ The containment check is on the CANONICALISED path, so `..` segments and
/// symlinks are both resolved before it is applied. Checking the raw string would
/// pass `a/../../etc/passwd`.
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
    // Rule 1: an API miss is a miss.
    if path.starts_with("/api/") {
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
