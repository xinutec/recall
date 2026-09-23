//! The recorders' lifecycle, appended to [`audiocore::capture_log`] in the
//! archive root: a source announcing itself, a phone connecting or dropping, a
//! pause, a resume, a producer cycled by the watchdog.
//!
//! Best-effort at every call site: the audio pump must never stall or die over
//! bookkeeping, so a failure is logged and swallowed.

use audiocore::capture_log::{self, Event};
use chrono::Utc;
use std::path::Path;

pub use capture_log::{INGEST_CONNECT, INGEST_DISCONNECT, PAUSE, PRODUCER_CYCLED, RESUME};

/// Say that `source` records here, and how (`coreaudio`, `tcp_pcm`). Written on
/// every start and every connect; the doctor takes the latest per source.
pub fn register(root: &Path, source: &str, kind: &str) {
    write(root, capture_log::REGISTER, source, Some(kind));
}

/// Append one lifecycle event, stamped now.
pub fn record(root: &Path, kind: &str, source: &str, detail: Option<&str>) {
    write(root, kind, source, detail);
}

fn write(root: &Path, kind: &str, source: &str, detail: Option<&str>) {
    let event = Event {
        utc: Utc::now(),
        kind: kind.to_owned(),
        source: Some(source.to_owned()),
        detail: detail.map(str::to_owned),
    };
    if let Err(err) = capture_log::append(root, &event) {
        tracing::error!(source, kind, error = %err, "could not record a capture event");
    }
}
