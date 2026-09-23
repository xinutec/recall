//! Every check that reads the archive volume, and the check on the reading
//! itself.
//!
//! Runs in the child process ([`crate::bounded`]): the capture log, the segment
//! stat walk and the pause marker all live on the archive volume, and any of
//! them can block indefinitely. Nothing here writes, because an abandoned child
//! may still be running after the parent gives up on it.

use crate::capture::{self, Beat, Recorder};
use crate::check::{Check, Verdict, check};
use crate::loss::{self, Event, Gap};
use crate::source::SourceKind;
use audiocore::capture_log;
use chrono::{DateTime, Duration, Utc};
use std::collections::BTreeMap;
use std::path::Path;
/// How far back the speech-loss reconciliation looks.
pub fn loss_window() -> Duration {
    Duration::hours(48)
}
/// The smallest uncovered active-capture stretch that counts as loss — below
/// this is the boundary slop of a pause recorded a beat after the last segment,
/// not real lost speech.
pub fn loss_min() -> Duration {
    Duration::minutes(2)
}
/// The trailing stretch the reconciler never judges: the newest segment (up to
/// 60s) is still being written, so coverage there is not yet known.
pub fn loss_settle() -> Duration {
    Duration::minutes(10)
}

/// How long the archive-reading half may take before it counts as not having
/// answered. A healthy run is ~1.5s; this must stay far under the agent's 300s
/// `StartInterval`, since launchd starts no new doctor while one is running.
pub fn archive_bound() -> Duration {
    Duration::seconds(60)
}
/// The reading that predicts the bound being hit: anything past a few seconds
/// means the volume is already contended.
pub fn archive_slow() -> Duration {
    Duration::seconds(10)
}

/// Could this machine read its own archive at all, and how long did it take?
/// Reported by the parent, so it survives a child stuck in disk wait.
///
/// Unanswered is `fail`, never `skip`: a skip reads as "not applicable". The
/// latency is carried as `value` so a slowing volume shows before it wedges.
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

/// The file the volume probe reads a page of: the Mac's retired meaning plane,
/// kept as an archive. Large and never written, so nothing keeps its pages warm.
pub const PROBE_FILE: &str = "recall.sqlite";

/// One page, the unit this probe is fixed at.
const PAGE: usize = 4096;

/// A page number under `pages`, varying run to run.
///
/// The clock, not a random crate: it only has to be unpredictable to the page
/// cache.
fn somewhere(pages: u64) -> u64 {
    if pages == 0 {
        return 0;
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| u64::from(since.subsec_nanos()))
        % pages
}

/// How long the fixed one-page read may take before it warns. Far above the
/// tenths of a second it costs on a well volume: this catches a mode, not
/// jitter.
pub fn volume_slow() -> Duration {
    Duration::seconds(4)
}

/// One page off the archive volume, timed: the only fixed-size read the doctor
/// does.
///
/// `archive answers` times the whole archive read (the capture log, the
/// uploader's state, a stat walk of every segment), so it slows both when the
/// volume is contended and
/// when the archive grows. This read never grows, so a rise in it belongs to
/// the disk. It samples one instant, at the start of the archive read: a stall
/// part-way through is invisible to it.
///
/// Kept beside `archive answers`, not folded into it: each keeps its own trend
/// history, and the difference between the two is the measurement.
pub fn volume_check(root: &Path) -> Check {
    use std::io::{Read, Seek, SeekFrom};
    let db = root.join(PROBE_FILE);
    let started = std::time::Instant::now();
    let read = std::fs::File::open(&db).and_then(|mut file| {
        // ⚠ A random page, never a fixed one: a page read every run stays in
        // the page cache, and would time 0.00s with the disk wedged.
        let pages = file.metadata()?.len() / PAGE as u64;
        file.seek(SeekFrom::Start(somewhere(pages) * PAGE as u64))?;
        let mut page = [0_u8; PAGE];
        // `read`, not `read_exact`: a file shorter than one page is not an
        // unreadable disk.
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

/// Every device that has registered here, by its latest registration, in id
/// order.
///
/// An unrecognised kind is dropped rather than guessed at: the grading rules
/// are judgements about specific kinds of hardware.
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

/// `(start, end)` of each of `source`'s segments on disk that ended at or after
/// `since`: the start from its name, the end from its mtime, which is when the
/// recorder last wrote to it. A zero-byte file is a stub that caught no audio
/// and covers nothing.
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

/// Reconcile the always-on mic's recorded coverage against the pause/resume
/// events over the recent window: an uncovered active stretch is capture
/// running and producing nothing.
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

/// Everything the child process reports. The `--collect` half of the doctor.
///
/// # Errors
/// If the capture log exists and cannot be read.
pub fn archive_checks(
    root: &Path,
    now: DateTime<Utc>,
    volume: Check,
) -> std::io::Result<Vec<Check>> {
    // The volume probe is taken by the caller and printed before the reads
    // below, so a child that hangs here has still said whether the disk
    // answered.
    let log = capture_log::read(root)?;
    // Registered recorders, not whatever directories exist.
    let sources = registered_devices(&log);
    let losses = speech_loss(root, &log, &sources, now);

    let paused_until = crate::agents::paused_until(root);
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
    // Quiet until audiod's uploader has run here (stage B): reads its state db.
    checks.extend(crate::delivery::delivery_checks(root, now));
    Ok(checks)
}

/// The runner's pulse, stamped in the archive root. It lives on the archive
/// volume so it cannot tick while the archive is unreachable, which is also
/// why it is read here, in the child.
pub fn read_beat(root: &Path) -> Option<Beat> {
    let text = std::fs::read_to_string(root.join("worker-heartbeat.json")).ok()?;
    serde_json::from_str(&text).ok()
}
