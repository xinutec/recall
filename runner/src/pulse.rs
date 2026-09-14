//! The archive pulse the doctor reads.

use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

/// Stamp the archive's pulse file, which the DOCTOR reads to answer "is this
/// Mac still turning audio into transcripts?".
///
/// ⚠ It lives ON the archive volume. A heartbeat that kept ticking while the
/// archive was unreachable would certify work it could not have done (#1412).
/// A failed write goes stale and the doctor says so, which is why this never
/// returns an error into the job path.
///
/// ⚠⚠ **AND IT NEVER BLOCKS THE JOB PATH EITHER, which is not the same thing
/// and cost two hours of the fleet's transcription on 2026-09-14.** The comment
/// above anticipated a write that FAILS. What happened is a write that never
/// returns: `/Volumes/Backup` is an external volume a launchd process has no
/// write grant for, and the `open()` inside `fs::write` HANGS rather than
/// answering `EPERM` (#1618). The runner finished one job, stamped its pulse,
/// and sat in that syscall for two hours with work queued — process up, shim
/// alive, every health check green, nothing in the log.
///
/// So the write happens on a background thread and the caller never waits.
/// A hung writer therefore costs a STALE pulse, which the doctor already
/// reports, instead of a stopped worker, which nothing reports. Bookkeeping
/// must not be able to stop the work it is bookkeeping for.
///
/// `rows` is the number of segments the shim returned, which is what the
/// worker's own `rows` counted: transcript rows produced by that unit of work.
pub fn stamp_pulse(path: Option<&Path>, started: DateTime<Utc>, rows: usize) {
    let Some(path) = path else { return };
    offer(path.to_path_buf(), body(started, Utc::now(), rows));
}

/// The pulse's exact shape — ONE spelling, shared by the background write and
/// the synchronous one, because the doctor parses it and a second spelling
/// would be a contract with two authors.
fn body(started: DateTime<Utc>, finished: DateTime<Utc>, rows: usize) -> String {
    serde_json::json!({
        "started": started.to_rfc3339_opts(chrono::SecondsFormat::Micros, false),
        "finished": finished.to_rfc3339_opts(chrono::SecondsFormat::Micros, false),
        // Via `std::time::Duration` rather than `num_milliseconds() as f64`,
        // which clippy rightly refuses: i64 does not fit in f64's mantissa. A
        // NEGATIVE span — the clock stepping back mid-job — cannot convert, and
        // becomes 0.0 rather than a fabricated number; the two stamps above
        // still say what actually happened.
        "seconds": (finished - started)
            .to_std()
            .map_or(0.0, |d| d.as_secs_f64()),
        "rows": rows,
    })
    .to_string()
}

/// Hand the pulse to the background writer, REPLACING any beat still waiting.
///
/// ⚠ Latest-wins, not a queue and not drop-newest. A heartbeat is superseded by
/// the next one, so a backlog of them is worthless — but so is keeping the
/// OLDEST of a burst, which is what a depth-1 channel does and what made the
/// contract tests here fail: 50 rapid stamps saturated the writer and every
/// later beat, including other tests', was thrown away. The freshest beat is
/// the only one worth having.
///
/// The lock is held only to swap the slot, never across the write, so a writer
/// stuck in `open()` cannot block a caller that is merely leaving a beat.
fn offer(path: PathBuf, body: String) {
    let slot = writer();
    if let Ok(mut held) = slot.0.lock() {
        *held = Some((path, body));
        slot.1.notify_one();
    }
}

type Slot = Arc<(Mutex<Option<(PathBuf, String)>>, Condvar)>;

/// The one background thread every stamp goes through.
///
/// It may block for ever inside `fs::write` — that is the whole point of it
/// being over here — so nothing joins it and nothing waits on it.
fn writer() -> &'static Slot {
    static WRITER: OnceLock<Slot> = OnceLock::new();
    WRITER.get_or_init(|| {
        let slot: Slot = Arc::new((Mutex::new(None), Condvar::new()));
        let mine = Arc::clone(&slot);
        std::thread::spawn(move || {
            loop {
                let Ok(mut held) = mine.0.lock() else { return };
                while held.is_none() {
                    let Ok(next) = mine.1.wait(held) else { return };
                    held = next;
                }
                let Some((path, body)) = held.take() else {
                    continue;
                };
                drop(held); // ⚠ BEFORE the write, which may never return.
                if let Err(err) = write_atomically(&path, &body) {
                    tracing::warn!(%err, path = %path.display(), "could not stamp the pulse");
                }
            }
        });
        slot
    })
}

/// Write the pulse HERE, on the calling thread, and say whether it landed.
///
/// For tests and for callers that genuinely want the answer. The agent does not
/// — see [`stamp_pulse`].
///
/// # Errors
/// Whatever the filesystem said.
pub fn stamp_now(path: &Path, started: DateTime<Utc>, rows: usize) -> std::io::Result<()> {
    write_atomically(path, &body(started, Utc::now(), rows))
}

/// Replace the pulse file in ONE step: write beside it, then rename over it.
///
/// ⚠ **`fs::write` truncates and then writes, so a reader can see an EMPTY or
/// half-written file** — and the reader here is the DOCTOR, which parses this as
/// JSON and would report a Mac whose pulse is corrupt rather than one that is
/// working. Found by a test that read mid-write and got
/// `EOF while parsing a value`; the race was there before this module wrote in
/// the background, it was just narrower.
///
/// `rename` within a directory is atomic, so a reader sees either the previous
/// beat or this one, never a fragment. Same pattern as `room::encode_flac`.
fn write_atomically(path: &Path, body: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("stamping");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, path)
}
