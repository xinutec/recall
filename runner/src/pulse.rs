//! The archive pulse the doctor reads.

use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

/// Stamp the archive's pulse file, which the doctor reads to answer "is this
/// Mac still turning audio into transcripts?".
///
/// The file lives on the archive volume, so a heartbeat cannot tick while the
/// archive is unreachable. A failed write leaves the pulse stale, which the
/// doctor reports, so no error reaches the job path.
///
/// ⚠ The write must never block the job path either. On an external volume
/// without a write grant for launchd processes, `open()` can hang instead of
/// returning `EPERM`. The write therefore runs on a background thread the
/// caller never waits for: a hung writer costs a stale pulse, not a stopped
/// worker.
///
/// `rows` is the number of segments the shim returned.
pub fn stamp_pulse(path: Option<&Path>, started: DateTime<Utc>, rows: usize) {
    let Some(path) = path else { return };
    offer(path.to_path_buf(), body(started, Utc::now(), rows));
}

/// The pulse's JSON, shared by the background and synchronous writes because
/// the doctor parses it.
fn body(started: DateTime<Utc>, finished: DateTime<Utc>, rows: usize) -> String {
    serde_json::json!({
        "started": started.to_rfc3339_opts(chrono::SecondsFormat::Micros, false),
        "finished": finished.to_rfc3339_opts(chrono::SecondsFormat::Micros, false),
        // Via `std::time::Duration`, since an i64 does not fit f64's mantissa.
        // A negative span (the clock stepped back) becomes 0.0; the two stamps
        // above still record what happened.
        "seconds": (finished - started)
            .to_std()
            .map_or(0.0, |d| d.as_secs_f64()),
        "rows": rows,
    })
    .to_string()
}

/// Hand the pulse to the background writer, replacing any beat still waiting.
///
/// Latest wins: a backlog of heartbeats is worthless, and a depth-1 channel
/// would keep the oldest of a burst instead of the freshest.
///
/// The lock is held only to swap the slot, never across the write, so a writer
/// stuck in `open()` cannot block a caller.
fn offer(path: PathBuf, body: String) {
    let slot = writer();
    if let Ok(mut held) = slot.0.lock() {
        held.next = Some((path, body));
        slot.1.notify_all();
    }
}

/// Wait, at most `limit`, for the last offered beat to land. Returns whether it
/// did.
///
/// For a process about to exit: the writer is a background thread, so `--once`
/// could otherwise exit before its beat is written, which a loaded machine does
/// (#1480; `a_runner_with_an_empty_queue_stamps_a_beat…` in
/// `tests/against_real_recalld.rs`). Bounded for the reason the writer is a
/// thread at all: `open()` on the archive volume can hang.
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

/// The one background thread every stamp goes through.
///
/// It may block for ever inside `fs::write`, so nothing joins it, and
/// [`settle`] waits for it only up to a limit.
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

/// Replace the pulse file in one step: write beside it, then rename over it.
/// `fs::write` truncates first, so the doctor could parse an empty or partial
/// file; `rename` within a directory is atomic.
fn write_atomically(path: &Path, body: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("stamping");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, path)
}
