//! A mic's minute segments laid end to end into runs of contiguous samples.
//! A segment's name gives its start to the second only, so a run's position
//! on the clock is approximate; alignment measures the rest.

use chrono::{DateTime, Utc};
use std::path::Path;

/// Samples that follow on from each other without a gap.
#[derive(Debug, Clone)]
pub struct Run {
    /// When the first sample was recorded, by the segment's name.
    pub start: DateTime<Utc>,
    pub samples: Vec<f32>,
}

/// `source`'s segments overlapping `[from, to)` under `root/source`, as runs.
/// A segment starting within `slack_s` of where the previous one ended
/// continues its run; anything else starts a new one.
pub fn runs(
    root: &Path,
    source: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    slack_s: f64,
) -> Vec<Run> {
    let mut segments: Vec<(DateTime<Utc>, std::path::PathBuf)> =
        audiocore::names::segment_glob(&root.join(source), source)
            .into_iter()
            .filter_map(|path| {
                let name = path.file_name()?.to_str()?;
                // A phone's own copy duplicates the host's cut of the same minute.
                if name.contains(".phone.") {
                    return None;
                }
                Some((audiocore::names::parse_segment_start(name)?, path))
            })
            .filter(|(start, _)| *start < to && *start + chrono::Duration::minutes(2) > from)
            .collect();
    segments.sort();
    let mut out: Vec<Run> = Vec::new();
    for (start, path) in segments {
        let Some(pcm) = audiocore::decode::decode_native_s16(&path) else {
            continue;
        };
        let samples = audiocore::decode::to_f32(&pcm);
        if let Some(last) = out.last_mut() {
            let ends = last.start.timestamp_micros() as f64 / 1e6
                + last.samples.len() as f64 / crate::RATE;
            let begins = start.timestamp_micros() as f64 / 1e6;
            if (begins - ends).abs() <= slack_s {
                last.samples.extend(samples);
                continue;
            }
        }
        out.push(Run { start, samples });
    }
    out
}
