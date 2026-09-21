//! Every check that has to READ the archive volume — and the check on the
//! reading itself.
//!
//! ⚠ This module runs in the CHILD process ([`crate::bounded`]). All of it can
//! block indefinitely: the store queries, the segment stat walk and the pause
//! marker all live on the archive volume, and on 2026-08-10 all three were in
//! uninterruptible disk wait at once. Keeping them behind one module keeps the
//! boundary honest — if a check belongs here it is unsafe to run in the
//! reporting process.
//!
//! The archive is opened READ-ONLY. The doctor observes; it must not be able to
//! write to the plane it is judging, and [`crate::bounded`]'s bargain requires
//! it: an abandoned child may still be running after the parent gave up on it,
//! so it has to be disposable.

use crate::blanked::{self, HiddenTurn};
use crate::capture::{self, Beat, Recorder};
use crate::check::{Check, Verdict, check};
use crate::loss::{self, Event, Gap};
use crate::source::SourceKind;
use chrono::{DateTime, Duration, Utc};
use rusqlite::{Connection, OpenFlags};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
/// How far back the speech-loss reconciliation looks.
pub fn loss_window() -> Duration {
    Duration::hours(48)
}
/// How far back the deaf-microphone comparison looks.
///
/// ⚠ **A long window hides the thing it is looking for.** The comparison only
/// speaks when peers heard a CONVERSATION, and a rate averaged over a night of
/// sleep falls below that: measured on the real archive, the same microphones
/// read 31-40 s/min over the fifteen minutes of an actual conversation and
/// 9-10 s/min once eight hours of quiet were folded in. Half an hour is long
/// enough to contain talking and short enough not to dilute it away.
pub fn deaf_window() -> Duration {
    Duration::minutes(30)
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
    let db = root.join("recall.sqlite");
    let started = std::time::Instant::now();
    let read = std::fs::File::open(&db).and_then(|mut file| {
        // ⚠ **A RANDOM page, never the first one.** The first page of this file
        // is read by every doctor run and by recalld continuously, so it is
        // always in the page cache — a probe on it times the CACHE and would
        // report 0.00s with the disk wedged, which is the one reading that must
        // never be possible here. The archive holds six figures of pages; a
        // fresh offset each run is almost certainly a real read.
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

/// See [`crate::blanked`] for why the count is gated by the detector.
pub fn blanked_check(blanked: usize) -> Check {
    check(
        "archive",
        "no blanked segments",
        if blanked == 0 {
            Verdict::Pass
        } else {
            Verdict::Fail
        },
        if blanked == 0 {
            "every transcribed segment shows turns".to_owned()
        } else {
            format!(
                "{blanked} segment(s) show NO turns where hidden ones exist — \
                 run `recall repair`"
            )
        },
        "0 blanked",
    )
    .trend(blanked as f64, "segments")
    .build()
}

/// The archive, opened read-only.
pub fn open(db: &Path) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
}

/// Registered sources — id and kind, in id order.
///
/// ⚠ An unrecognised kind is DROPPED rather than guessed at. A source whose
/// kind this build has never heard of cannot be graded (the always-on rule is a
/// judgement about a specific kind of hardware), and inventing a verdict for it
/// would be worse than the missing line.
pub fn source_rows(conn: &Connection) -> rusqlite::Result<Vec<(String, SourceKind)>> {
    let mut stmt = conn.prepare("SELECT id, kind FROM sources ORDER BY id")?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, kind)| Some((id, SourceKind::parse(&kind)?)))
        .collect())
}

/// Capture events at or after `since`, oldest-first.
pub fn capture_events_since(
    conn: &Connection,
    since: DateTime<Utc>,
) -> rusqlite::Result<Vec<Event>> {
    let mut stmt = conn.prepare(
        "SELECT utc, kind, source_id FROM capture_events WHERE utc >= ?1 ORDER BY utc, id",
    )?;
    let rows = stmt
        .query_map([python_iso(since)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|(utc, kind, source_id)| {
            Some(Event {
                utc: crate::instant::parse(&utc)?,
                kind,
                source_id,
            })
        })
        .collect())
}

/// `(start, end)` of `source`'s audio segments ending at or after `since` — the
/// recorded coverage a loss check reconciles against the pause/resume events.
pub fn audio_segment_intervals(
    conn: &Connection,
    source: &str,
    since: DateTime<Utc>,
) -> rusqlite::Result<Vec<(DateTime<Utc>, DateTime<Utc>)>> {
    let mut stmt = conn.prepare(
        "SELECT start_utc, end_utc FROM audio_segments
         WHERE source_id = ?1 AND end_utc >= ?2 ORDER BY start_utc",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![source, python_iso(since)], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|(start, end)| {
            Some((crate::instant::parse(&start)?, crate::instant::parse(&end)?))
        })
        .collect())
}

/// Segments that once had turns and now show none — the ones a refine emptied,
/// gated by the speech detector.
pub fn blanked_segments(conn: &Connection) -> rusqlite::Result<usize> {
    let silent: BTreeSet<i64> = conn
        .prepare("SELECT id FROM audio_segments WHERE speech_s = 0.0")?
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<_>>()?;

    // Superseded turns are excluded: those were properly replaced, and their
    // replacement stands.
    let mut stmt = conn.prepare(
        "SELECT t.audio_segment_id, t.id, t.hidden_reason, t.text
           FROM transcript_segments t
          WHERE t.audio_segment_id IS NOT NULL
            AND t.hidden_reason IS NOT NULL
            AND t.superseded_by IS NULL
            AND NOT EXISTS (
                SELECT 1 FROM transcript_segments v
                 WHERE v.audio_segment_id = t.audio_segment_id
                   AND v.superseded_by IS NULL AND v.hidden_reason IS NULL)
          ORDER BY t.audio_segment_id, t.id",
    )?;
    let mut by_segment: BTreeMap<i64, Vec<HiddenTurn>> = BTreeMap::new();
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            HiddenTurn {
                id: row.get(1)?,
                hidden_reason: row.get(2)?,
                text: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
            },
        ))
    })?;
    for row in rows {
        let (audio_id, turn) = row?;
        by_segment.entry(audio_id).or_default().push(turn);
    }

    Ok(by_segment
        .into_iter()
        .filter(|(audio_id, _)| !silent.contains(audio_id))
        .filter(|(_, turns)| {
            let restore = blanked::last_generation(turns);
            if restore.is_empty() {
                return false;
            }
            let texts: Vec<&str> = turns
                .iter()
                .filter(|t| restore.contains(&t.id))
                .map(|t| t.text.as_str())
                .collect();
            blanked::any_restorable(&texts)
        })
        .count())
}

/// The spelling `datetime.isoformat()` writes, for the TEXT comparisons the
/// timestamp columns are ordered and filtered by.
///
/// ⚠ Not cosmetic. `start_utc` is compared and ordered as TEXT, so two
/// spellings of the same moment are two different values to every query here.
pub fn python_iso(when: DateTime<Utc>) -> String {
    let format = if when.timestamp_subsec_micros() == 0 {
        chrono::SecondsFormat::Secs
    } else {
        chrono::SecondsFormat::Micros
    };
    when.to_rfc3339_opts(format, false)
}

/// Reconcile the always-on mic's recorded coverage against the pause/resume
/// events over the recent window: uncovered active stretches (capture active, no
/// audio) plus the dead windows *per device* — telling a deliberate pause from
/// lost speech, and one broken microphone from a broken house.
fn speech_loss(
    conn: &Connection,
    sources: &[(String, SourceKind)],
    now: DateTime<Utc>,
) -> rusqlite::Result<(Vec<Gap>, BTreeMap<String, usize>)> {
    let since = now - loss_window();
    let events = capture_events_since(conn, since)?;
    let mut dead: BTreeMap<String, usize> = BTreeMap::new();
    for event in events.iter().filter(|e| e.kind == loss::DEAD_WINDOW) {
        // source_id is nullable in the schema. Loss the archive cannot
        // attribute is still loss, so it gets its own bucket instead of being
        // dropped.
        let bucket = event
            .source_id
            .clone()
            .unwrap_or_else(|| "unattributed".to_owned());
        *dead.entry(bucket).or_default() += 1;
    }
    let mut losses = Vec::new();
    for (source_id, kind) in sources {
        if *kind != capture::ALWAYS_ON {
            continue;
        }
        let intervals = audio_segment_intervals(conn, source_id, since)?;
        losses.extend(loss::uncovered_loss(
            &intervals,
            &events,
            source_id,
            now,
            loss_min(),
            loss_settle(),
        ));
    }
    Ok((losses, dead))
}

/// Everything the child process reports. The `--collect` half of the doctor.
///
/// ⚠ It took a `fleet_configured` flag until 2026-09-17, to gate a `fleet mirror
/// complete` check on `pushed_utc`. That column's writer (`sync_push`) is
/// deleted, so the count could only ever be zero and the check could only ever
/// say the archive was safely replicated — the one claim worth being sure of.
/// `delivery_checks` makes the same promise on evidence that is still written:
/// every file on disk against audiod's own upload state.
pub fn archive_checks(
    root: &Path,
    now: DateTime<Utc>,
    volume: Check,
) -> rusqlite::Result<Vec<Check>> {
    // ⚠ The probe is taken by the CALLER and handed in, so it can be SAID
    // before the queries below run. Computed here it would be lost on exactly
    // the run it exists for: a child that hangs never returns these checks at
    // all, so the one reading that could say whether the DISK answered went
    // down with it.
    let conn = open(&root.join("recall.sqlite"))?;
    // Registered recorders, not whatever directories exist: a mic the household
    // actually uses is one the archive knows about. Devices only — an imported
    // meeting is a source but has no recorder that could stop or lose speech,
    // and a row per meeting would bury the four microphones that matter.
    let sources: Vec<(String, SourceKind)> = source_rows(&conn)?
        .into_iter()
        .filter(|(_, kind)| kind.is_device())
        .collect();
    let (losses, dead_windows) = speech_loss(&conn, &sources, now)?;
    let blanked = blanked_segments(&conn)?;
    let heard = crate::deaf::heard_between(&conn, &sources, now - deaf_window(), now)?;
    drop(conn);

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
    checks.extend(loss::loss_checks(
        &losses,
        &dead_windows,
        &sources,
        loss_window(),
    ));
    checks.push(crate::deaf::deaf_check(&heard));
    checks.push(capture::worker_check(
        beat.as_ref(),
        now,
        capture::worker_slow(),
        capture::worker_stopped(),
    ));
    // Quiet until audiod's uploader has run here (stage B): reads its state db.
    checks.extend(crate::delivery::delivery_checks(root, now));
    checks.push(blanked_check(blanked));
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
