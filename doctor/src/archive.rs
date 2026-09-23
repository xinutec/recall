//! Every check that has to READ the archive volume — and the check on the
//! reading itself.
//!
//! ⚠ This module runs in the CHILD process ([`crate::bounded`]). All of it can
//! block indefinitely: the capture log, the segment stat walk and the pause
//! marker all live on the archive volume, and on 2026-08-10 all three were in
//! uninterruptible disk wait at once. Keeping them behind one module keeps the
//! boundary honest — if a check belongs here it is unsafe to run in the
//! reporting process.
//!
//! Nothing here writes. The doctor observes, and [`crate::bounded`]'s bargain
//! requires it: an abandoned child may still be running after the parent gave
//! up on it, so it has to be disposable.

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
/// The store always trails a running capture: an in-progress segment (up to
/// 60s) plus the worker's min-age guard (120s) plus its pass cadence. Coverage
/// inside this trailing stretch is unknowable, so the reconciler never judges it.
pub fn loss_settle() -> Duration {
    Duration::minutes(10)
}

/// How long the archive-reading half of the doctor may take before it is
/// treated as not having answered. A healthy run is ~1.5s end to end, so this
/// is forty times the work — and it has to stay far under the agent's 300s
/// `StartInterval`, because `KeepAlive = false` means launchd will not start the
/// next doctor while this one is still going: one wedged run silences every run
/// after it.
pub fn archive_bound() -> Duration {
    Duration::seconds(60)
}
/// The reading that predicts the bound being hit. During the 2026-08-10
/// starvation a two-table `COUNT(*)` alone took 4m19s, so the interesting range
/// is not near 1.5s; anything past a few seconds means the volume is already
/// contended.
pub fn archive_slow() -> Duration {
    Duration::seconds(10)
}

/// Could this machine read its own archive at all — and how long did it take?
///
/// Every other check in this module presumes the archive answered. On
/// 2026-08-10 it did not, for over an hour, and the doctor did not report that:
/// it lives on the volume it checks, so it went into uninterruptible disk wait
/// alongside the worker and the sync (#709). This is the check that survives
/// that, because the archive is read in a child process the parent abandons and
/// the timeout is reported from off the disk.
///
/// ⚠ **Unanswered is `fail`, never `skip`.** A skip reads as "not applicable",
/// and nothing is more applicable than the archive being unreachable — the June
/// lesson was that a silence which looks deliberate is how a fault survives for
/// weeks. The latency is carried as `value` so the trend is visible while it is
/// still only slow, which is the only warning anyone gets before it wedges.
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

/// The file the volume probe reads a page of: the Mac's meaning plane, retired
/// on 2026-09-23 and kept as an archive. Large, and never written again, so
/// nothing else keeps its pages warm.
pub const PROBE_FILE: &str = "recall.sqlite";

/// One page, the unit this probe is fixed at.
const PAGE: usize = 4096;

/// A page number under `pages`, varying run to run.
///
/// ⓘ The clock, not a random crate: this needs to be UNPREDICTABLE TO THE CACHE,
/// not unpredictable to an adversary, and a dependency for that would be a
/// dependency for nothing.
fn somewhere(pages: u64) -> u64 {
    if pages == 0 {
        return 0;
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| u64::from(since.subsec_nanos()))
        % pages
}

/// How long a FIXED read of the volume may take before it is worth saying so.
/// Four seconds is absurd for one page off a working disk and is deliberately
/// far above the tenths this costs when the volume is well — the point is to
/// catch a mode, not to grade jitter.
pub fn volume_slow() -> Duration {
    Duration::seconds(4)
}

/// One page off the archive volume, timed. **The only fixed-size read the
/// doctor does.**
///
/// ⚠ **This exists because `archive answers` cannot separate two causes.** That
/// check times the whole archive read — six queries and a directory listing —
/// so it gets slower when the volume is contended AND when the archive simply
/// grows, and the value it trends cannot say which. Measured over its own
/// history, the FASTEST read of the day moved from 0.05 s to seconds, in a
/// floor that flips between two modes and holds for hours; a volume that stops
/// answering adds a tail and does not raise a floor, so at least one of the two
/// is not the fault that check was built for.
///
/// This one does not grow. Its size is one page, today and after another year
/// of recording, so a rise in it belongs to the DISK — which is exactly the
/// discrimination the whole question turns on.
///
/// ⚠ It samples ONE INSTANT, at the start of the archive read. A volume that
/// stalls part-way through the queries is invisible to it, so a healthy reading
/// beside a slow `archive answers` narrows the cause without closing it.
///
/// ⚠ Kept BESIDE `archive answers`, never folded into it: that check has its
/// own history under its own name, and the difference between the two trends is
/// the measurement. Renaming it would spend the history to say the same thing.
pub fn volume_check(root: &Path) -> Check {
    use std::io::{Read, Seek, SeekFrom};
    let db = root.join(PROBE_FILE);
    let started = std::time::Instant::now();
    let read = std::fs::File::open(&db).and_then(|mut file| {
        // ⚠ **A RANDOM page, never the first one.** A page read every run stays
        // in the page cache, and a probe on it times the CACHE — 0.00s with the
        // disk wedged, the one reading that must never be possible here. The
        // file holds six figures of pages; a fresh offset each run is almost
        // certainly a real read.
        let pages = file.metadata()?.len() / PAGE as u64;
        file.seek(SeekFrom::Start(somewhere(pages) * PAGE as u64))?;
        let mut page = [0_u8; PAGE];
        // ⚠ `read`, not `read_exact`. An archive smaller than one page is not a
        // stalled volume, and `read_exact` would report the disk as UNREADABLE
        // for it — a fault invented by the instrument.
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
/// ⚠ An unrecognised kind is DROPPED rather than guessed at. A source whose
/// kind this build has never heard of cannot be graded (the always-on rule is a
/// judgement about a specific kind of hardware), and inventing a verdict for it
/// would be worse than the missing line.
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
    // ⚠ The probe is taken by the CALLER and handed in, so it can be SAID
    // before the reads below run. Computed here it would be lost on exactly
    // the run it exists for: a child that hangs never returns these checks at
    // all, so the one reading that could say whether the DISK answered went
    // down with it.
    let log = capture_log::read(root)?;
    // Registered recorders, not whatever directories exist: a mic the household
    // actually uses is one that has announced itself here.
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

/// The worker's pulse, stamped in the archive root.
///
/// It lives on the archive volume rather than off it, deliberately: a heartbeat
/// that kept ticking while the archive was unreachable would be worse than none,
/// since the thing it certifies is work done *on* that archive. That is also why
/// it is read here, in the child, and not in the reporting process.
pub fn read_beat(root: &Path) -> Option<Beat> {
    let text = std::fs::read_to_string(root.join("worker-heartbeat.json")).ok()?;
    serde_json::from_str(&text).ok()
}
