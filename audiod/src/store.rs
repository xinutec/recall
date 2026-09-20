//! The daemon's only two contacts with recall.sqlite: registering a source on
//! its first handshake, and the durable capture-event telemetry. Port of
//! `Store.register_source` + `Store.add_capture_event` (the SQL is copied, not
//! re-derived — the Python migrations own the schema).
//!
//! Both are best-effort at every call site: the audio pump must never stall or
//! die over bookkeeping, so a failure is logged and swallowed. The connection
//! is opened per write, exactly as the Python server does — the writes are
//! rare (a connect, a disconnect) and holding no handle means holding no lock.

use chrono::{DateTime, SecondsFormat, Utc};
use std::path::Path;
use std::time::Duration;

pub const KIND_INGEST_CONNECT: &str = "ingest_connect";
pub const KIND_INGEST_DISCONNECT: &str = "ingest_disconnect";
pub const KIND_PAUSE: &str = "pause";
pub const KIND_RESUME: &str = "resume";
pub const KIND_PRODUCER_CYCLED: &str = "producer_cycled";

/// Whether this machine has a meaning plane at all.
///
/// ⚠ **ABSENT IS A SHAPE, NOT A FAULT.** A store-and-forward recorder — geb —
/// has no `recall.sqlite` and is not supposed to: its segments reach the meaning
/// plane on Isis through `audiod upload` with verified receipts
/// (docs/architecture.md). The same call is right on the Mac, which
/// runs beside a real archive, and meaningless there. Reporting it as an ERROR
/// on every start of a correctly configured recorder is how a log gets ignored.
///
/// ⚠ **Do not "fix" this by creating the file.** A local meaning plane the fleet
/// would have to merge is exactly what store-and-forward exists to avoid.
#[must_use]
pub fn has_meaning_plane(root: &Path) -> bool {
    root.join("recall.sqlite").is_file()
}

fn open(root: &Path) -> rusqlite::Result<rusqlite::Connection> {
    // READ_WRITE without CREATE: the migrations own the file, and half-creating
    // one here would put an empty archive where a recorder expects none.
    let conn = rusqlite::Connection::open_with_flags(
        root.join("recall.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
    )?;
    conn.busy_timeout(Duration::from_secs(5))?;
    Ok(conn)
}

/// Auto-register a device the first time it connects, by the id it announced.
/// Idempotent. The name is preserved *unless* it is still the placeholder
/// (equal to the id): a name the user chose in the UI is theirs, and
/// re-registering a phone must never rename it back.
pub fn register_source(root: &Path, source_id: &str) {
    register_source_kind(root, source_id, "tcp_pcm");
}

/// The same registration under an explicit kind — the capture agent registers
/// the local mic as `coreaudio`, correcting any kind a worker guessed.
pub fn register_source_kind(root: &Path, source_id: &str, kind: &str) {
    if !has_meaning_plane(root) {
        tracing::debug!(
            source = source_id,
            root = %root.display(),
            "no meaning plane here — store-and-forward recorder, nothing to register"
        );
        return;
    }
    let result = open(root).and_then(|conn| {
        conn.execute(
            "INSERT INTO sources (id, name, kind, port) VALUES (?1, ?2, ?3, NULL)
             ON CONFLICT(id) DO UPDATE SET
                 kind = excluded.kind,
                 port = excluded.port,
                 name = CASE WHEN sources.name = sources.id
                             THEN excluded.name ELSE sources.name END",
            (source_id, source_id, kind),
        )
    });
    if let Err(err) = result {
        tracing::error!(source = source_id, error = %err, "could not register source");
    }
}

/// Append an immutable capture-lifecycle event — the durable record that tells
/// a deliberate gap apart from silently lost audio.
pub fn add_capture_event(
    root: &Path,
    kind: &str,
    utc: DateTime<Utc>,
    source_id: &str,
    detail: Option<&str>,
) {
    if !has_meaning_plane(root) {
        // Same shape as registration: the durable record of this recorder's
        // lifecycle is the fleet's, reached by delivery rather than written here.
        tracing::debug!(kind, source = source_id, "no meaning plane here");
        return;
    }
    let result = open(root).and_then(|conn| {
        conn.execute(
            "INSERT INTO capture_events (utc, kind, source_id, detail) VALUES (?1, ?2, ?3, ?4)",
            (
                // +00:00 rather than Z: the Python readers parse either, but
                // matching Store's own isoformat keeps the rows grep-alike.
                utc.to_rfc3339_opts(SecondsFormat::Micros, false),
                kind,
                source_id,
                detail,
            ),
        )
    });
    if let Err(err) = result {
        tracing::error!(source = source_id, kind, error = %err, "ingest: could not record capture event");
    }
}
