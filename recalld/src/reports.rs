//! What the browser tells the server: error reports and the activity trace.
//!
//! `/api/log` appends a browser error to a file (the phone has no readable
//! console); `/api/telemetry` logs what a person did, which otherwise never
//! reaches the server.
//!
//! `one_line` is a security boundary: client text goes into log lines, where a
//! newline could forge a `client-event` line.

use axum::Json;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

/// So one POST cannot flood the log.
const MAX_EVENTS: usize = 100;
/// In characters, so a multi-byte glyph is never split.
const MAX_LABEL: usize = 160;

/// Invisible characters that can reorder or disguise a rendered line: bidi
/// overrides and zero-width marks. Control characters and line separators are
/// covered by `is_control` and `is_whitespace`.
const REORDERING: &[char] = &[
    '\u{00AD}', // soft hyphen
    '\u{200B}', '\u{200C}', '\u{200D}', '\u{200E}', '\u{200F}', // zero-width + LRM/RLM
    '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}', // bidi embedding/override
    '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}', // bidi isolates
    '\u{FEFF}', // zero-width no-break space
];

/// Flatten client text to one harmless log field: anything that could break
/// or re-render a line becomes a space, whitespace collapses, then it is cut
/// to `max_len` characters.
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

/// One thing that happened in the client. The types are a shipped app's
/// contract: `at` is epoch milliseconds, a number.
#[derive(Debug, Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct TelemetryEvent {
    pub kind: String,
    pub path: String,
    /// Null for a navigation, a control's visible text for a tap; always sent.
    pub label: Option<String>,
    pub at: i64,
}

/// The line `/api/log` appends; only a stack's first line is kept.
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
    /// Where `/api/log` appends. Telemetry goes to the tracing log instead, to
    /// interleave with the requests.
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
        // A failed write must not fail the request.
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

pub async fn telemetry_route(Json(events): Json<Vec<TelemetryEvent>>) -> Response {
    for e in events.iter().take(MAX_EVENTS) {
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
