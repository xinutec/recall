//! Shared plumbing for the browsing-tier route handlers.
//!
//! Every route in [`crate::reads`], [`crate::labels`] and [`crate::work`] does
//! the same two things: move a blocking `SQLite` call off the async runtime, and
//! turn a failure into a 500 without leaking why.
//!
//! ⚠ The blocking pool is not optional: `rusqlite` is synchronous, and a query
//! on the request thread stalls every other request, ingest included.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// The body a fault answers with.
///
/// Deliberately says nothing about what failed: no caller branches on it. The
/// log line names the route and carries the error.
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
#[expect(clippy::result_large_err, reason = "the Err is the HTTP response")]
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
/// `{"ok": true}`. Nothing reads the field; the app branches on the status.
#[derive(Serialize, ts_rs::TS)]
#[ts(export, rename = "Ok")]
pub struct Ack {
    ok: bool,
}

pub fn ack() -> Response {
    Json(Ack { ok: true }).into_response()
}

/// What a write that minted a row answers with.
#[derive(Serialize, ts_rs::TS)]
#[ts(export, rename = "CorrectResult")]
pub struct NewId {
    #[serde(rename = "newId")]
    pub new_id: i64,
}
