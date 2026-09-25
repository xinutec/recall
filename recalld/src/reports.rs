//! What the browser tells the server: error reports and the activity trace.
//!
//! Neither route touches a database. `/api/log` appends a browser-side error to
//! a file, because the phone has no readable console; `/api/telemetry` logs what
//! a person did (a tap that hit a cache, a disabled control), which otherwise
//! never reaches the server.
//!
//! ⚠ `one_line` is a security boundary: client text goes verbatim into log
//! lines, and a newline in it could forge whole `client-event` lines.

use axum::Json;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

/// A per-batch cap, so a buggy client cannot turn one POST into a log flood.
const MAX_EVENTS: usize = 100;
/// A per-label cap, counted in CHARACTERS rather than bytes so a multi-byte
/// glyph is never split in half.
const MAX_LABEL: usize = 160;

/// Invisible characters that can reorder or disguise a rendered line: bidi
/// overrides and zero-width marks. They cannot forge a newline, but they can
/// make a log line read as something it does not say.
///
/// An explicit list rather than the whole Cf category: `char::is_control`
/// covers Cc and `char::is_whitespace` covers U+2028/U+2029, leaving this set.
const REORDERING: &[char] = &[
    '\u{00AD}', // soft hyphen
    '\u{200B}', '\u{200C}', '\u{200D}', '\u{200E}', '\u{200F}', // zero-width + LRM/RLM
    '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}', // bidi embedding/override
    '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}', // bidi isolates
    '\u{FEFF}', // zero-width no-break space
];

/// Flatten a client-supplied label to a single harmless log field.
///
/// Everything that could break or re-render a line becomes a space, then runs of
/// whitespace collapse to one. Truncation is last and counts characters.
#[must_use]
pub fn one_line(label: &str, max_len: usize) -> String {
    let unbroken: String = label
        .chars()
        .map(|c| {
            if c.is_control() || c.is_whitespace() || REORDERING.contains(&c) {
                ' '
            } else {
                c
            }
        })
        .collect();
    unbroken
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_len)
        .collect()
}

#[derive(Debug, Deserialize)]
pub struct ClientLog {
    pub level: String,
    #[serde(default)]
    pub url: Option<String>,
    pub message: String,
    #[serde(default)]
    pub stack: Option<String>,
}

/// One thing that happened in the client.
///
/// ⚠ The field types are the contract with a shipped app. `at` is the client's
/// clock in epoch milliseconds, a number; typed as a string, every batch fails
/// to deserialise. `path` is required.
#[derive(Debug, Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct TelemetryEvent {
    pub kind: String,
    pub path: String,
    /// Null for a nav event, a control's visible text for a tap — present either
    /// way, so no `serde(default)`: the client sends the key on every event.
    pub label: Option<String>,
    pub at: i64,
}

/// The line `/api/log` appends, split out to be testable without a filesystem.
/// Only the first line of a stack is kept, so one client error cannot write a
/// hundred lines.
#[must_use]
pub fn log_line(stamp: &str, entry: &ClientLog) -> String {
    let mut line = format!(
        "{stamp} [{}] {} {}",
        one_line(&entry.level, MAX_LABEL),
        one_line(entry.url.as_deref().unwrap_or("-"), MAX_LABEL),
        one_line(&entry.message, MAX_LABEL * 4),
    );
    if let Some(stack) = entry.stack.as_deref()
        && let Some(first) = stack.lines().next()
    {
        line.push_str("\n    ");
        line.push_str(&one_line(first, MAX_LABEL * 4));
    }
    line
}

#[derive(Serialize)]
struct Ok_ {
    ok: bool,
}

fn ok() -> Response {
    Json(Ok_ { ok: true }).into_response()
}

#[derive(Clone, Debug)]
pub struct Reports {
    /// Where `/api/log` appends. The activity trace goes to the tracing
    /// subscriber instead, so a session interleaves with the request log and
    /// reads as one timeline.
    pub log_path: std::path::PathBuf,
}

pub async fn log_route(
    axum::extract::State(st): axum::extract::State<std::sync::Arc<Reports>>,
    Json(body): Json<ClientLog>,
) -> Response {
    let stamp = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%:z")
        .to_string();
    let line = log_line(&stamp, &body);
    let path = st.log_path.clone();
    let _ = tokio::task::spawn_blocking(move || {
        use std::io::Write;
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // A failed write must not fail the request: losing the report is better
        // than turning it into a second error.
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            Ok(mut fh) => {
                let _ = writeln!(fh, "{line}");
            }
            Err(e) => tracing::warn!("client log write failed: {e}"),
        }
    })
    .await;
    ok()
}

#[allow(
    clippy::unused_async,
    reason = "axum's handler signature; nothing here awaits"
)]
pub async fn telemetry_route(Json(events): Json<Vec<TelemetryEvent>>) -> Response {
    for e in events.iter().take(MAX_EVENTS) {
        // `at` is an integer, so it needs no flattening — only the free-text
        // fields can carry a newline.
        tracing::info!(
            "client-event kind={} path={} label={} at={}",
            one_line(&e.kind, MAX_LABEL),
            one_line(&e.path, MAX_LABEL),
            one_line(e.label.as_deref().unwrap_or(""), MAX_LABEL),
            e.at,
        );
    }
    ok()
}
