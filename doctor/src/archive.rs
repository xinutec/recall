//! Every check that reads the archive volume, run in the child process
//! ([`crate::bounded`]): any of these reads can block indefinitely. Nothing
//! here writes, since an abandoned child may still be running.

use crate::capture::{self, Beat, Recorder};
use crate::check::{Check, Verdict, check};
use crate::loss::{self, Event, Gap};
use crate::source::SourceKind;
use audiocore::capture_log;
use chrono::{DateTime, Duration, Utc};
use std::collections::BTreeMap;
use std::path::Path;

/// How far back the speech-loss check looks.
pub fn loss_window() -> Duration {
    Duration::hours(48)
}
/// The shortest uncovered stretch that counts as loss; shorter is boundary
/// slop.
pub fn loss_min() -> Duration {
    Duration::minutes(2)
}
/// The recent stretch not judged, while its segment may still be written.
pub fn loss_settle() -> Duration {
    Duration::minutes(10)
}

/// How long the child may take before it counts as not answering. A healthy
/// run is ~1.5 s; this must stay well under the agent's 300 s interval.
pub fn archive_bound() -> Duration {
    Duration::seconds(60)
}
/// Slower than this, the volume is already contended.
pub fn archive_slow() -> Duration {
    Duration::seconds(10)
}

/// Did the archive answer, and how fast? Built by the parent, so it is
/// reported even when the child hangs. No answer fails rather than skips; the
/// latency is the trend, so a slowing volume shows before it wedges.
pub fn archive_check(seconds: Option<f64>, detail: &str) -> Check {
    let slow = archive_slow().num_seconds() as f64;
    let expected = format!("the archive read in under {slow:.0}s");
    let Some(seconds) = seconds else {
        return check(
            "archive",
            "archive answers",
            Verdict::Fail,
            if detail.is_empty() {
                format!("no answer in {}s", archive_bound().num_seconds())
            } else {
                detail.to_owned()
            },
            expected,
        )
        .build();
    };
    if !detail.is_empty() {
        return check(
            "archive",
            "archive answers",
            Verdict::Fail,
            format!("{detail} (after {seconds:.1}s)"),
            expected,
        )
        .build();
    }
    check(
        "archive",
        "archive answers",
        if seconds >= slow {
            Verdict::Warn
        } else {
            Verdict::Pass
        },
        if seconds < slow {
            format!("read in {seconds:.1}s")
        } else {
            format!("read in {seconds:.1}s — something else owns the disk")
        },
        expected,
    )
    .trend((seconds * 100.0).round() / 100.0, "s")
    .build()
}

/// The file the volume probe reads a page of: the Mac's retired meaning plane.
/// Large and never written, so its pages are rarely cached.
pub const PROBE_FILE: &str = "recall.sqlite";

const PAGE: usize = 4096;

/// A page number under `pages` from the clock's nanoseconds, varying by run.
fn somewhere(pages: u64) -> u64 {
    if pages == 0 {
        return 0;
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| u64::from(since.subsec_nanos()))
        % pages
}

/// A well volume reads a page in tenths of a second; this catches a mode, not
/// jitter.
pub fn volume_slow() -> Duration {
    Duration::seconds(4)
}

/// One page off the archive volume, timed.
///
/// `archive answers` times the whole read, which also grows with the archive;
/// this read is fixed, so a rise in it is the disk. It samples only the start
/// of the archive read. A separate check, so each keeps its own trend.
pub fn volume_check(root: &Path) -> Check {
    use std::io::{Read, Seek, SeekFrom};
    let db = root.join(PROBE_FILE);
    let started = std::time::Instant::now();
    let read = std::fs::File::open(&db).and_then(|mut file| {
        // A page read every run would stay cached and time 0.00 s with the
        // disk wedged.
        let pages = file.metadata()?.len() / PAGE as u64;
        file.seek(SeekFrom::Start(somewhere(pages) * PAGE as u64))?;
        let mut page = [0_u8; PAGE];
        // Not `read_exact`: a file under one page is not an unreadable disk.
        file.read(&mut page).map(|_| ())
    });
    let seconds = started.elapsed().as_secs_f64();
    let slow = volume_slow().num_seconds() as f64;
    let expected = format!("one page off the volume in under {slow:.0}s");
    if let Err(err) = read {
        return check(
            "archive",
            "the volume answers",
            Verdict::Fail,
            format!("cannot read the archive: {err}"),
            expected,
        )
        .build();
    }
    check(
        "archive",
        "the volume answers",
        if seconds >= slow {
            Verdict::Warn
        } else {
            Verdict::Pass
        },
        format!("one page in {seconds:.2}s"),
        expected,
    )
    .trend((seconds * 1000.0).round() / 1000.0, "s")
    .build()
}

/// Every device registered here, by its latest registration, in id order. An
/// unrecognised kind is dropped: the grading rules are per kind.
#[must_use]
pub fn registered_devices(events: &[capture_log::Event]) -> Vec<(String, SourceKind)> {
    let mut latest: BTreeMap<&str, &str> = BTreeMap::new();
    for event in events.iter().filter(|e| e.kind == capture_log::REGISTER) {
        if let (Some(source), Some(kind)) = (event.source.as_deref(), event.detail.as_deref()) {
            latest.insert(source, kind);
        }
    }
    latest
        .into_iter()
        .filter_map(|(id, kind)| Some((id.to_owned(), SourceKind::parse(kind)?)))
        .filter(|(_, kind)| kind.is_device())
        .collect()
}

/// `(start, end)` of each non-empty segment of `source` ending at or after
/// `since`: the start from its name, the end from its mtime.
#[must_use]
pub fn recorded_intervals(
    root: &Path,
    source: &str,
    since: DateTime<Utc>,
) -> Vec<(DateTime<Utc>, DateTime<Utc>)> {
    let Ok(entries) = std::fs::read_dir(root.join(source)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(start) = audiocore::names::parse_segment_start(&name.to_string_lossy()) else {
            continue;
        };
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        let end = DateTime::<Utc>::from(modified);
        if meta.len() > 0 && end >= since {
            out.push((start, end));
        }
    }
    out
}

/// The always-on mics' lost speech over [`loss_window`].
fn speech_loss(
    root: &Path,
    log: &[capture_log::Event],
    sources: &[(String, SourceKind)],
    now: DateTime<Utc>,
) -> Vec<Gap> {
    let since = now - loss_window();
    let events: Vec<Event> = log
        .iter()
        .filter(|e| e.utc >= since)
        .map(|e| Event {
            utc: e.utc,
            kind: e.kind.clone(),
            source_id: e.source.clone(),
        })
        .collect();
    let mut losses = Vec::new();
    for (source_id, kind) in sources {
        if *kind != capture::ALWAYS_ON {
            continue;
        }
        losses.extend(loss::uncovered_loss(
            &recorded_intervals(root, source_id, since),
            &events,
            source_id,
            now,
            loss_min(),
            loss_settle(),
        ));
    }
    losses
}

/// What `--collect` prints.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Collected {
    /// How long the child took, excluding its startup.
    pub seconds: f64,
    pub checks: Vec<Check>,
    /// The pause file's instant, for the parent's checks.
    pub paused_until: Option<String>,
}

/// Every check the child reports. `volume` is probed by the caller and printed
/// first, so a child that hangs here has still said whether the disk answered.
///
/// # Errors
/// If the capture log exists and cannot be read.
pub fn archive_checks(
    root: &Path,
    now: DateTime<Utc>,
    volume: Check,
    paused_until: Option<DateTime<Utc>>,
) -> std::io::Result<Vec<Check>> {
    let log = capture_log::read(root)?;
    // Registered recorders, not whatever directories exist.
    let sources = registered_devices(&log);
    let losses = speech_loss(root, &log, &sources, now);

    let recorders: Vec<Recorder> = capture::recorders_on_disk(root, &sources);
    let beat = read_beat(root);

    let mut checks = vec![volume];
    checks.extend(capture::capture_checks(
        &recorders,
        now,
        paused_until,
        capture::silent_after(),
    ));
    checks.extend(loss::loss_checks(&losses, &sources, loss_window()));
    checks.push(capture::worker_check(
        beat.as_ref(),
        now,
        capture::worker_slow(),
        capture::worker_stopped(),
    ));
    checks.extend(crate::delivery::delivery_checks(root, now));
    Ok(checks)
}

/// The runner's pulse, in the archive root.
pub fn read_beat(root: &Path) -> Option<Beat> {
    let text = std::fs::read_to_string(root.join("worker-heartbeat.json")).ok()?;
    serde_json::from_str(&text).ok()
}
