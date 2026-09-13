//! The archive pulse the doctor reads.

use chrono::{DateTime, Utc};
use std::path::Path;

/// Stamp the archive's pulse file, which the DOCTOR reads to answer "is this
/// Mac still turning audio into transcripts?".
///
/// ⚠ **Inherited from `worker.py`, which is gone (#1538).** The worker wrote
/// `worker-heartbeat.json` at the archive root after every pass, and
/// `doctor::capture::worker_check` grades the time since one completed. Deleting
/// the worker without writing this would not have removed the check — it would
/// have made it fail for ever, which is worse than no check at all.
///
/// ⚠ **It lives ON the archive volume, deliberately.** A heartbeat that kept
/// ticking while the archive was unreachable would certify work it could not
/// have done (#1412 — that volume intermittently stops answering). If the write
/// fails, the pulse goes stale and the doctor says so; that is the correct
/// outcome and why this never returns an error into the job path.
///
/// `rows` is the number of segments the shim returned, which is what the
/// worker's own `rows` counted: transcript rows produced by that unit of work.
pub fn stamp_pulse(path: Option<&Path>, started: DateTime<Utc>, rows: usize) {
    let Some(path) = path else { return };
    let finished = Utc::now();
    let beat = serde_json::json!({
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
    });
    if let Err(err) = std::fs::write(path, beat.to_string()) {
        tracing::warn!(%err, path = %path.display(), "could not stamp the pulse");
    }
}
