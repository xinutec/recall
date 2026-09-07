//! Shared plumbing for the browsing-tier route handlers.
//!
//! Every route in [`crate::reads`], [`crate::labels`] and [`crate::work`] does
//! the same two things before it can answer: move a blocking `SQLite` call off
//! the async runtime, and turn a failure into a 500 without leaking why. Written
//! out per route that was twenty-six copies of the same three match arms, which
//! is twenty-six places for one of them to drift.
//!
//! ⚠ **The blocking pool is not optional here.** `rusqlite` is synchronous, and a
//! query run on the request thread stalls every other browsing request for its
//! duration — including the recorders' ingest, which shares this runtime.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// The body a fault answers with.
///
/// ⚠ Deliberately says nothing about which query failed. It replaces three
/// near-identical predecessors ("read failed", "write failed", "clip failed")
/// that no caller ever branched on — the frontend shows a generic message and
/// the tests assert status codes. What the distinction was actually worth is in
/// the log line, which names the route and carries the error.
const FAULT: &str = "request failed";

fn fault() -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, FAULT).into_response()
}

/// Log a fault and answer 500 — for routes whose own error type says more than
/// `rusqlite::Error` does, so they cannot go through [`blocking`].
pub fn faulted(what: &str, err: &dyn std::fmt::Display) -> Response {
    tracing::warn!("{what} failed: {err}");
    fault()
}

/// Run a blocking database closure, or a 500 describing nothing.
///
/// `what` names the route for the log only. Both failure modes land in the same
/// place on purpose: a panicked task and a failed query are equally a fault of
/// ours, and the caller can act on neither.
pub async fn blocking<T, F>(what: &'static str, f: F) -> Result<T, Response>
where
    F: FnOnce() -> rusqlite::Result<T> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(err)) => {
            tracing::warn!("{what} failed: {err}");
            Err(fault())
        }
        // spawn_blocking cannot be cancelled by dropping the handle, so in
        // practice this is a panic in the closure. Display says which.
        Err(err) => {
            tracing::warn!("{what} task failed: {err}");
            Err(fault())
        }
    }
}

/// [`blocking`], serialised — the shape almost every read route wants.
pub async fn json<T, F>(what: &'static str, f: F) -> Response
where
    F: FnOnce() -> rusqlite::Result<T> + Send + 'static,
    T: Serialize + Send + 'static,
{
    match blocking(what, f).await {
        Ok(value) => Json(value).into_response(),
        Err(response) => response,
    }
}

/// What a write route answers with when it has nothing to return.
///
/// Copies the Python's `{"ok": true}` rather than improving on it, so a route
/// can move between the two implementations without the frontend seeing a
/// change. Nothing reads the field — the app branches on the status — which is
/// exactly why it is cheap to keep identical.
#[derive(Serialize)]
pub struct Ack {
    ok: bool,
}

pub fn ack() -> Response {
    Json(Ack { ok: true }).into_response()
}
