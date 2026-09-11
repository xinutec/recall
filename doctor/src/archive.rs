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

/// The live tier writes its turns under this ASR model name.
const LIVE_MODEL: &str = "live";

/// In-flight slack for the fleet-mirror check: the sync timer runs every 120s
/// and the mirror-completion queue drains 500 a pass, so anything processed an
/// hour ago and still unpushed means the push has actually stopped.
pub fn mirror_slack() -> Duration {
    Duration::hours(1)
}
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

/// Is the fleet's copy of the archive complete?
///
/// The invariant since the mirror-completion push: every processed segment
/// reaches the fleet within a couple of sync passes. A count stuck above zero
/// (beyond the in-flight slack) means the mirror has silently stopped — the same
/// class of failure as a stalled backup, and it gets the same verdict: `fail`,
/// because "if the Mac dies the archive lives on Isis" is only true while this
/// is zero.
pub fn mirror_check(unmirrored: usize, slack: Duration) -> Check {
    check(
        "sync",
        "fleet mirror complete",
        if unmirrored == 0 {
            Verdict::Pass
        } else {
            Verdict::Fail
        },
        if unmirrored == 0 {
            "every processed segment mirrored".to_owned()
        } else {
            format!("{unmirrored} processed segment(s) not on the fleet")
        },
        format!(
            "0 unmirrored older than {:.0}m",
            slack.num_seconds() as f64 / 60.0
        ),
    )
    .trend(unmirrored as f64, "segments")
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

/// Processed segments that have never reached the fleet (`pushed_utc` unset).
///
/// Covers what the turn-watermark push cannot: a speechless segment mints no
/// turn ids, so it never synced and the fleet's quiet review could never sweep
/// it.
pub fn unmirrored_count(
    conn: &Connection,
    older_than: DateTime<Utc>,
    limit: usize,
) -> rusqlite::Result<usize> {
    let count: usize = conn.query_row(
        "SELECT count(*) FROM (
             SELECT id FROM audio_segments
             WHERE transcribed_utc IS NOT NULL AND pushed_utc IS NULL
               AND transcribed_utc < ?1
             ORDER BY id LIMIT ?2)",
        rusqlite::params![python_iso(older_than), limit],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// When the live tier last produced a turn, or `None` if it never has.
///
/// Deliberately ignores `hidden_reason`: a live turn the archive has since
/// reconciled still proves live was working when it wrote it.
pub fn newest_live_turn(conn: &Connection) -> rusqlite::Result<Option<DateTime<Utc>>> {
    let newest: Option<String> = conn.query_row(
        "SELECT max(start_utc) FROM transcript_segments WHERE asr_model = ?1",
        [LIVE_MODEL],
        |row| row.get(0),
    )?;
    Ok(newest.as_deref().and_then(crate::instant::parse))
}

/// What the device recorders delivered in `[since, until)`, and how much of it
/// the speech scanner has measured — the evidence [`crate::capture::live_check`]
/// needs to tell a quiet house from a broken live tier.
///
/// ⚠ Delivered and scanned are counted in ONE pass over the same rows, so they
/// cannot disagree about which segments were in the window. Counting them
/// separately would let a segment land between the two queries and read as
/// delivered-but-unscanned forever.
pub fn window_audio(
    conn: &Connection,
    sources: &[(String, crate::source::SourceKind)],
    since: DateTime<Utc>,
    until: DateTime<Utc>,
) -> rusqlite::Result<crate::capture::WindowAudio> {
    let mut out = crate::capture::WindowAudio {
        delivered_s: 0.0,
        scanned_s: 0.0,
        speech_s: 0.0,
    };
    for (source, kind) in sources {
        if !kind.is_device() {
            continue;
        }
        let mut stmt = conn.prepare(
            "SELECT start_utc, end_utc, speech_s FROM audio_segments
             WHERE source_id = ?1 AND start_utc >= ?2 AND start_utc < ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![source, python_iso(since), python_iso(until)],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<f64>>(2)?,
                ))
            },
        )?;
        for row in rows {
            let (start, end, speech) = row?;
            let (Some(start), Some(end)) =
                (crate::instant::parse(&start), crate::instant::parse(&end))
            else {
                continue;
            };
            let seconds = (end - start).num_milliseconds() as f64 / 1000.0;
            if seconds <= 0.0 {
                continue;
            }
            out.delivered_s += seconds;
            if let Some(speech) = speech {
                out.scanned_s += seconds;
                out.speech_s += speech;
            }
        }
    }
    Ok(out)
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
pub fn archive_checks(
    root: &Path,
    now: DateTime<Utc>,
    fleet_configured: bool,
) -> rusqlite::Result<Vec<Check>> {
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
    let unmirrored = unmirrored_count(&conn, now - mirror_slack(), 10_000)?;
    let blanked = blanked_segments(&conn)?;
    let heard = crate::deaf::heard_between(&conn, &sources, now - deaf_window(), now)?;
    let newest_live = newest_live_turn(&conn)?;
    let live_window = window_audio(&conn, &sources, now - capture::live_quiet(), now)?;
    drop(conn);

    let paused_until = crate::agents::paused_until(root);
    let recorders: Vec<Recorder> = capture::recorders_on_disk(root, &sources);
    let beat = read_beat(root);

    let mut checks =
        capture::capture_checks(&recorders, now, paused_until, capture::silent_after());
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
    checks.push(capture::live_check(
        newest_live,
        now,
        paused_until,
        capture::live_quiet(),
        live_window,
    ));
    // The fleet mirror only exists when the split is on (RECALL_SYNC_TOKEN
    // set); a stock LAN-only deployment has no fleet to be incomplete against.
    if fleet_configured {
        checks.push(mirror_check(unmirrored, mirror_slack()));
    }
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
