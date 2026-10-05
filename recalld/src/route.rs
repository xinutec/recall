//! Route plumbing: run a blocking `SQLite` call off the async runtime (on the
//! request thread it would stall every other request), and turn a failure into
//! a 500 that says nothing.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// The log line carries the route and the error; the answer neither.
const FAULT: &str = "request failed";

fn fault() -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, FAULT).into_response()
}

/// Where faults are kept, one JSON line each, for the doctor to count
/// (`GET /sync/record/health`). Set once, at startup.
static KEPT_IN: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Keep every fault in `path` from now on. The first call wins.
pub fn keep_faults_in(path: std::path::PathBuf) {
    let _ = KEPT_IN.set(path);
}

/// Log a fault, and keep it where the doctor looks. A fault log that cannot
/// be written is only logged.
fn record(what: &str, err: &dyn std::fmt::Display) {
    tracing::warn!("{what} failed: {err}");
    let Some(path) = KEPT_IN.get() else {
        return;
    };
    let line = audiocore::record_health::Fault {
        utc: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        what: what.to_owned(),
        error: err.to_string(),
    };
    let written = serde_json::to_string(&line)
        .map_err(std::io::Error::other)
        .and_then(|json| {
            use std::io::Write;
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .and_then(|mut file| writeln!(file, "{json}"))
        });
    if let Err(err) = written {
        tracing::warn!("cannot keep a fault in {}: {err}", path.display());
    }
}

/// Log a fault and answer 500, for routes with their own error type.
pub fn faulted(what: &str, err: &dyn std::fmt::Display) -> Response {
    record(what, err);
    fault()
}

/// Run a blocking database closure; a failed query or a panic is a 500.
/// `what` names the route in the log.
#[expect(clippy::result_large_err, reason = "the Err is the HTTP response")]
pub async fn blocking<T, F>(what: &'static str, f: F) -> Result<T, Response>
where
    F: FnOnce() -> rusqlite::Result<T> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(err)) => {
            record(what, &err);
            Err(fault())
        }
        // A panic: spawn_blocking is not cancelled by dropping the handle.
        Err(err) => {
            record(&format!("{what} task"), &err);
            Err(fault())
        }
    }
}

/// [`blocking`], answered as JSON.
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

/// `{"ok": true}`, for a write with nothing to return. The app reads the status.
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
