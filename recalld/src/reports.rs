//! What the browser tells the server: error reports and the activity trace.
//!
//! `/api/log` appends a browser error to a file (the phone has no readable
//! console); `/api/telemetry` keeps what a person did, one JSON line per event
//! in a file on the volume, so which features are used can be counted across
//! restarts, and logs it too.
//!
//! `one_line` is a security boundary: client text goes into log lines, where a
//! newline could forge a `client-event` line.

use axum::Json;
use axum::response::Response;
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

/// One kept event, as `client-events.jsonl` holds it. A tap's label is a
/// control's visible text, which can be a speaker's name: runtime data on the
/// volume, never in the repository.
#[derive(Debug, Serialize)]
struct Kept<'a> {
    kind: String,
    path: String,
    label: Option<String>,
    at: i64,
    received: &'a str,
}

/// The line `/api/telemetry` appends for `event`: JSON, so it is one line
/// whatever the label holds.
#[must_use]
pub fn event_line(received: &str, event: &TelemetryEvent) -> String {
    let kept = Kept {
        kind: one_line(&event.kind, MAX_LABEL),
        path: one_line(&event.path, MAX_LABEL),
        label: event.label.as_deref().map(|l| one_line(l, MAX_LABEL)),
        at: event.at,
        received,
    };
    serde_json::to_string(&kept).expect("strings and numbers serialise")
}

#[derive(Clone, Debug)]
pub struct Reports {
    /// Where `/api/log` appends.
    pub log_path: std::path::PathBuf,
    /// Where `/api/telemetry` appends.
    pub events_path: std::path::PathBuf,
}

/// Append `lines` to `path`. A failed write must not fail the request.
async fn append(path: std::path::PathBuf, lines: Vec<String>) {
    let _ = tokio::task::spawn_blocking(move || {
        use std::io::Write;
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            Ok(mut fh) => {
                for line in lines {
                    let _ = writeln!(fh, "{line}");
                }
            }
            Err(e) => tracing::warn!("{} write failed: {e}", path.display()),
        }
    })
    .await;
}

pub async fn log_route(
    axum::extract::State(st): axum::extract::State<std::sync::Arc<Reports>>,
    Json(body): Json<ClientLog>,
) -> Response {
    let stamp = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%:z")
        .to_string();
    append(st.log_path.clone(), vec![log_line(&stamp, &body)]).await;
    crate::route::ack()
}

pub async fn telemetry_route(
    axum::extract::State(st): axum::extract::State<std::sync::Arc<Reports>>,
    Json(events): Json<Vec<TelemetryEvent>>,
) -> Response {
    let received = audiocore::instant::python_isoformat_utc(chrono::Utc::now());
    let kept: Vec<String> = events
        .iter()
        .take(MAX_EVENTS)
        .map(|e| event_line(&received, e))
        .collect();
    append(st.events_path.clone(), kept).await;
    for e in events.iter().take(MAX_EVENTS) {
        tracing::info!(
            "client-event kind={} path={} label={} at={}",
            one_line(&e.kind, MAX_LABEL),
            one_line(&e.path, MAX_LABEL),
            one_line(e.label.as_deref().unwrap_or(""), MAX_LABEL),
            e.at,
        );
    }
    crate::route::ack()
}
