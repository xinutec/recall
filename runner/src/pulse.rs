//! The archive pulse the doctor reads.

use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

/// Stamp the archive's pulse, by which the doctor sees this Mac still turning
/// audio into transcripts. On the archive volume, so it cannot tick while the
/// archive is unreachable; a failure just leaves it stale.
///
/// On a background thread: on an external volume `open()` can hang instead of
/// returning `EPERM`, and that must not stop the worker. `rows` is the
/// segments the shim returned.
pub fn stamp_pulse(path: Option<&Path>, started: DateTime<Utc>, rows: usize) {
    let Some(path) = path else { return };
    offer(path.to_path_buf(), body(started, Utc::now(), rows));
}

/// The pulse's JSON, which the doctor parses.
fn body(started: DateTime<Utc>, finished: DateTime<Utc>, rows: usize) -> String {
    serde_json::json!({
        "started": started.to_rfc3339_opts(chrono::SecondsFormat::Micros, false),
        "finished": finished.to_rfc3339_opts(chrono::SecondsFormat::Micros, false),
        // A clock stepped back gives 0.0; the stamps above still tell.
        "seconds": (finished - started)
            .to_std()
            .map_or(0.0, |d| d.as_secs_f64()),
        "rows": rows,
    })
    .to_string()
}

/// Hand the pulse to the background writer, replacing any beat still waiting
/// (latest wins). The lock is held only to swap the slot.
fn offer(path: PathBuf, body: String) {
    let slot = writer();
    if let Ok(mut held) = slot.0.lock() {
        held.next = Some((path, body));
        slot.1.notify_all();
    }
}

/// Wait at most `limit` for the last beat to land, before exiting (#1480).
/// Bounded, since `open()` on the archive volume can hang.
#[must_use]
pub fn settle(limit: std::time::Duration) -> bool {
    let slot = writer();
    let Ok(held) = slot.0.lock() else {
        return false;
    };
    slot.1
        .wait_timeout_while(held, limit, |state| state.next.is_some() || state.writing)
        .is_ok_and(|(_, waited)| !waited.timed_out())
}

/// What the writer has to do: the beat waiting, and whether one is being written.
#[derive(Default)]
struct State {
    next: Option<(PathBuf, String)>,
    writing: bool,
}

type Slot = Arc<(Mutex<State>, Condvar)>;

/// The background writer. It may block for ever in `fs::write`, so nothing
/// joins it.
fn writer() -> &'static Slot {
    static WRITER: OnceLock<Slot> = OnceLock::new();
    WRITER.get_or_init(|| {
        let slot: Slot = Arc::new((Mutex::new(State::default()), Condvar::new()));
        let mine = Arc::clone(&slot);
        std::thread::spawn(move || {
            loop {
                let Ok(mut held) = mine.0.lock() else { return };
                while held.next.is_none() {
                    let Ok(next) = mine.1.wait(held) else { return };
                    held = next;
                }
                let Some((path, body)) = held.next.take() else {
                    continue;
                };
                held.writing = true;
                drop(held); // Before the write, which may never return.
                if let Err(err) = write_atomically(&path, &body) {
                    tracing::warn!(%err, path = %path.display(), "could not stamp the pulse");
                }
                let Ok(mut held) = mine.0.lock() else { return };
                held.writing = false;
                mine.1.notify_all();
            }
        });
        slot
    })
}

/// Write the pulse on the calling thread and report whether it landed. For
/// tests; the agent uses [`stamp_pulse`].
///
/// # Errors
/// Whatever the filesystem said.
pub fn stamp_now(path: &Path, started: DateTime<Utc>, rows: usize) -> std::io::Result<()> {
    write_atomically(path, &body(started, Utc::now(), rows))
}

/// Write beside the pulse file and rename over it, so the doctor never reads a
/// partial one.
fn write_atomically(path: &Path, body: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("stamping");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, path)
}
