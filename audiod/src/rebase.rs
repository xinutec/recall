//! Capture-time segment naming: shift a connection's arrival-stamped segment
//! names back to the phone's announced capture epoch.

use audiocore::names::{parse_segment_start, segment_glob};
use chrono::{DateTime, Duration, Utc};
use std::collections::HashSet;
use std::hash::BuildHasher;
use std::path::Path;

/// A phone clock this far from the server's is not synchronised at all (no
/// NTP); applying it would smear segment names arbitrarily. Real buffering
/// delays are seconds; real NTP skew is milliseconds.
const MAX_EPOCH_SKEW_S: f64 = 600.0;

const TS_FORMAT: &str = "%Y%m%dT%H%M%S";

/// Seconds to shift this connection's segment names: capture minus arrival.
/// Negative is physical (the phone buffered). Positive can only be skew, and
/// renaming forward could pass ffmpeg's open segment, so it clamps to 0.0.
/// `None` without an epoch, or one too far off to trust.
pub fn connection_offset(epoch: Option<f64>, first_byte_wall: f64) -> Option<f64> {
    let offset = epoch? - first_byte_wall;
    if offset.abs() > MAX_EPOCH_SKEW_S {
        tracing::warn!(
            offset_s = offset,
            "ingest: epoch far from arrival — phone clock untrusted, keeping arrival-stamped names"
        );
        return None;
    }
    Some(offset.min(0.0))
}

/// Rename this connection's closed segments from arrival to capture time,
/// returning the renames.
///
/// - The newest file is ffmpeg's open segment: untouched unless
///   `include_newest`.
/// - A file stamped before `since` is an earlier connection's.
/// - `done` holds every name handled, created ones too, so none shifts twice.
/// - A segment whose new name is taken keeps its arrival name.
pub fn rebase_segment_names<S: BuildHasher>(
    out_dir: &Path,
    source_id: &str,
    offset_s: f64,
    done: &mut HashSet<String, S>,
    since: DateTime<Utc>,
    include_newest: bool,
) -> Vec<(String, String)> {
    let shift = Duration::seconds(offset_s.round() as i64);
    if shift.is_zero() {
        return Vec::new();
    }
    let mut renamed = Vec::new();
    let mut files = segment_glob(out_dir, source_id);
    if !include_newest {
        files.pop(); // the last file is ffmpeg's open segment
    }
    for path in files {
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(String::from) else {
            continue;
        };
        if !done.insert(name.clone()) {
            continue; // already handled this sweep or a previous one
        }
        let Some(start) = parse_segment_start(&name) else {
            continue; // not a timestamped segment; leave it be
        };
        if start < since {
            continue; // an earlier connection's segment; not ours to move
        }
        let suffix = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{e}"))
            .unwrap_or_default();
        let corrected = start + shift;
        let new_name = format!("{source_id}-{}{suffix}", corrected.format(TS_FORMAT));
        if new_name == name {
            continue;
        }
        let target = out_dir.join(&new_name);
        if target.exists() {
            tracing::warn!(
                old = name,
                new = new_name,
                "ingest: segment keeps its arrival name — corrected slot is taken"
            );
            continue;
        }
        if let Err(err) = std::fs::rename(&path, &target) {
            // Never let bookkeeping drop a stream.
            tracing::warn!(old = name, error = %err, "ingest: could not rebase segment");
            continue;
        }
        done.insert(new_name.clone()); // its own next sweep must not shift it again
        renamed.push((name, new_name));
    }
    if !renamed.is_empty() {
        tracing::info!(
            source = source_id,
            count = renamed.len(),
            shift_s = offset_s.round(),
            "ingest: rebased segment names to capture time"
        );
    }
    renamed
}
