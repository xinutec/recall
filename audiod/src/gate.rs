//! The capture gate: the one file every recording agent checks before it
//! records, holding a [`Window`], and the local commands that write it.
//!
//! Off unless a person opened a window. A missing or unreadable file is a
//! fault, and a fault records: the doctor reports it, and no call is lost to a
//! broken file.

use audiocore::capture::Window;
pub use audiocore::capture::{GATE_FILE, read_gate as window};
use chrono::{DateTime, Duration, Utc};
use std::path::Path;

/// Whether to record at `now`. A fault records.
pub fn records(root: &Path, now: DateTime<Utc>) -> bool {
    window(root).is_none_or(|w| w.records(now))
}

/// Replace the window. Written aside and renamed in, so no reader sees half a
/// file, which would read as a fault and record.
///
/// # Errors
/// If the file cannot be written, which means the change did not take.
pub fn write(root: &Path, window: Window) -> std::io::Result<()> {
    let aside = root.join(format!(".{GATE_FILE}.new"));
    std::fs::write(&aside, window.spell())?;
    std::fs::rename(aside, root.join(GATE_FILE))
}

/// Open a window of `minutes` from `now`, capped as every window is.
///
/// # Errors
/// As [`write()`].
pub fn record(root: &Path, now: DateTime<Utc>, minutes: i64) -> std::io::Result<Window> {
    let opened = Window::open(now, now + Duration::minutes(minutes));
    write(root, opened)?;
    Ok(opened)
}

/// Apply `how` (pause or unpause) to the window as it stands. A fault reads
/// as off here: whoever presses wants it to stop, not to record on.
///
/// # Errors
/// As [`write()`].
pub fn change(
    root: &Path,
    now: DateTime<Utc>,
    how: fn(Window) -> Window,
) -> std::io::Result<Window> {
    let changed = how(window(root).unwrap_or(Window::Off).at(now));
    write(root, changed)?;
    Ok(changed)
}
