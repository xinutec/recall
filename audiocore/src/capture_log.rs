//! The capture lifecycle on a recording machine: an append-only log in the
//! archive root, written by audiod and read by the doctor.
//!
//! One JSON object per line. Appending is the only write, so a crash can at
//! worst leave a torn last line, which [`read`] skips.
//!
//! ⚠ **It replaced the Mac's `recall.sqlite` on 2026-09-23.** That database had
//! no schema owner once the Python ladder went; this file's format is
//! [`Event`], so the writer and the reader cannot disagree about it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use std::path::Path;

/// The log's name in the archive root.
pub const FILE: &str = "capture-events.jsonl";

/// A source announced itself; `detail` is its kind (`coreaudio`, `tcp_pcm`).
pub const REGISTER: &str = "register";
pub const INGEST_CONNECT: &str = "ingest_connect";
pub const INGEST_DISCONNECT: &str = "ingest_disconnect";
/// Capture was deliberately stopped; the loss check does not count what follows.
pub const PAUSE: &str = "pause";
/// Capture was (re)started; from here until the next pause, audio is expected.
pub const RESUME: &str = "resume";
pub const PRODUCER_CYCLED: &str = "producer_cycled";

/// One lifecycle event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    #[serde(
        serialize_with = "instant_ser",
        deserialize_with = "crate::instant::de"
    )]
    pub utc: DateTime<Utc>,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

fn instant_ser<S: serde::Serializer>(at: &DateTime<Utc>, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&crate::instant::python_isoformat_utc(*at))
}

/// Append one event.
///
/// ⚠ **One `write_all` of the whole line**, on a file opened for append: the
/// agents that share this log (the recorder, the phone ingest) each write
/// short lines, and a single append-mode write of that size lands whole.
///
/// # Errors
/// If the file cannot be opened or written.
pub fn append(root: &Path, event: &Event) -> std::io::Result<()> {
    let mut line = serde_json::to_string(event).map_err(std::io::Error::other)?;
    line.push('\n');
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join(FILE))?
        .write_all(line.as_bytes())
}

/// Every event in the log, in the order written. An absent log is an empty
/// one: a machine that has recorded nothing yet has no events.
///
/// # Errors
/// If the file exists and cannot be read.
pub fn read(root: &Path) -> std::io::Result<Vec<Event>> {
    let file = match std::fs::File::open(root.join(FILE)) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let mut out = Vec::new();
    for line in std::io::BufReader::new(file).lines() {
        if let Ok(event) = serde_json::from_str(&line?) {
            out.push(event);
        }
    }
    Ok(out)
}
