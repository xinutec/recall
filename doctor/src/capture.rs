//! Is the recording actually recording? — the check recall never had.
//!
//! Capture can die. On 22 June it crash-looped: fourteen start attempts
//! between 01:05 and 03:10, an hour and a half of nothing recorded, and
//! *nobody knew*. It was found three weeks later by diffing the filesystem
//! against the database by hand. launchd restarts capture when it dies
//! (`KeepAlive`), which is why a persistent fault becomes a loop rather than a
//! stop — and a loop looks, from the outside, exactly like a quiet house.
//!
//! So this asks the only question that matters: **is audio still landing on
//! disk?**
//!
//! It reads the *filesystem*, not the database, deliberately. A segment file
//! appears every 60 seconds while capture lives; the transcription pipeline
//! behind it can be hours behind without any of that meaning the microphone
//! stopped. Asking the pipeline whether the recorder is alive conflates two
//! failures that need different answers.
//!
//! The verdicts are not uniform, because the recorders are not:
//!
//! * the **always-on mic** (the USB condenser, wired to this machine) has no
//!   excuse for silence. If it stops, recording has stopped: `fail`.
//! * a **phone** is carried out of the house, runs out of battery, has its app
//!   closed. Its silence is normal life, so it `warn`s — loud enough to see,
//!   too quiet to cry wolf.
//! * **every source silent at once** is not three coincidences. It is the
//!   capture process, or the machine, and it is the loudest thing this module
//!   can say.
//!
//! A *paused* recording is not a broken one: everything reports `skip`, with
//! the resume time shown, so a deliberate pause reads as deliberate.

use crate::check::{Check, Verdict, check};
use crate::source::SourceKind;
use chrono::{DateTime, Duration, Utc};
use std::path::Path;

/// A live capture writes a segment file every 60 seconds. Two missed rotations
/// is noise (a slow disk, a rotation straddling the check); five is not.
pub fn silent_after() -> Duration {
    Duration::minutes(5)
}

/// The always-on mic is the one that must never be silent — it is wired to the
/// machine doing the recording. A phone leaves the house; this does not.
pub const ALWAYS_ON: SourceKind = SourceKind::CoreAudio;

/// One microphone, and when audio last landed on disk from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorder {
    pub source_id: String,
    pub kind: SourceKind,
    pub last_audio: Option<DateTime<Utc>>,
}

/// Minutes, to one decimal — the unit every duration in a check is reported in.
pub fn minutes(since: Duration) -> f64 {
    (since.num_milliseconds() as f64 / 60_000.0 * 10.0).round() / 10.0
}

/// How a pause is spelled in `observed`: `datetime.isoformat(timespec="minutes")`.
fn to_the_minute(when: DateTime<Utc>) -> String {
    when.format("%Y-%m-%dT%H:%M%:z").to_string()
}

/// What fleetwatch should be told about the recording, right now.
///
/// Pure — the filesystem read happens in [`recorders_on_disk`], so the rules
/// that decide whether a household has stopped being recorded are testable
/// without a microphone.
pub fn capture_checks(
    recorders: &[Recorder],
    now: DateTime<Utc>,
    paused_until: Option<DateTime<Utc>>,
    silent_after: Duration,
) -> Vec<Check> {
    let expected = format!("audio within {:.0} min", minutes(silent_after));

    if let Some(until) = paused_until
        && until > now
    {
        // Deliberate. Not a fault, and it must never page anyone — but it is
        // shown, because a pause nobody remembers is how a week of memory goes
        // missing.
        return vec![
            check(
                "capture",
                "recording",
                Verdict::Skip,
                format!("paused until {}", to_the_minute(until)),
                expected,
            )
            .build(),
        ];
    }

    let mut checks = Vec::new();
    let mut silent = 0usize;
    let mut ordered: Vec<&Recorder> = recorders.iter().collect();
    ordered.sort_by(|a, b| a.source_id.cmp(&b.source_id));

    for recorder in ordered {
        let since = recorder.last_audio.map(|last| now - last);
        let quiet = since.is_none_or(|s| s >= silent_after);
        if quiet {
            silent += 1;
        }
        let verdict = if !quiet {
            Verdict::Pass
        } else if recorder.kind == ALWAYS_ON {
            Verdict::Fail // wired to this machine: silence means it stopped
        } else {
            Verdict::Warn // a phone: out of the house, flat battery, app closed
        };
        let observed = match since {
            None => "no audio ever recorded".to_owned(),
            Some(s) => format!("last audio {:.1} min ago", minutes(s)),
        };
        let mut builder = check(
            "capture",
            recorder.source_id.clone(),
            verdict,
            observed,
            expected.clone(),
        );
        if let Some(s) = since {
            builder = builder.trend(minutes(s), "min");
        }
        checks.push(builder.build());
    }

    // The summary check, and the one that catches what actually happened in
    // June: three microphones do not fall silent together by coincidence. That
    // is the capture process, or the machine it runs on.
    let everything = !recorders.is_empty() && silent == recorders.len();
    let observed = if recorders.is_empty() {
        "no recorders found".to_owned()
    } else if everything {
        "every microphone is silent — capture is not running".to_owned()
    } else {
        format!(
            "{}/{} microphones live",
            recorders.len() - silent,
            recorders.len()
        )
    };
    checks.push(
        check(
            "capture",
            "recording",
            if everything || recorders.is_empty() {
                Verdict::Fail
            } else {
                Verdict::Pass
            },
            observed,
            expected,
        )
        .build(),
    );
    checks
}

/// When each microphone last wrote audio, read from the archive directory
/// itself.
///
/// The newest *non-empty* file's mtime, not the database: capture writing to
/// disk is the fact under test, and the pipeline that indexes those files can
/// be far behind without the microphone having missed a second. Zero-byte
/// segments are skipped — capture can run and roll a fresh file every segment
/// while the device delivers only digital silence (a coreaudio startup
/// dead-window), and those empty stubs must not read as "recording": counting
/// them is how a 13-minute dead window looked healthy.
pub fn recorders_on_disk(root: &Path, sources: &[(String, SourceKind)]) -> Vec<Recorder> {
    sources
        .iter()
        .map(|(source_id, kind)| {
            let mut newest: Option<std::time::SystemTime> = None;
            // The segment grammar is `<source_id>-*`, sorted by name (which is
            // chronological); the mtime is what is compared, so the order does
            // not matter here and the directory is read as it comes.
            if let Ok(entries) = std::fs::read_dir(root.join(source_id)) {
                let prefix = format!("{source_id}-");
                for entry in entries.flatten() {
                    if !entry.file_name().to_string_lossy().starts_with(&prefix) {
                        continue;
                    }
                    // vanished mid-scan; the next pass will see it
                    let Ok(meta) = entry.metadata() else { continue };
                    if meta.len() == 0 {
                        continue; // dead stub — a file rolled, no audio caught
                    }
                    let Ok(modified) = meta.modified() else {
                        continue;
                    };
                    if newest.is_none_or(|best| modified > best) {
                        newest = Some(modified);
                    }
                }
            }
            Recorder {
                source_id: source_id.clone(),
                kind: *kind,
                last_audio: newest.map(DateTime::<Utc>::from),
            }
        })
        .collect()
}

/// The live tier is the one that answers a question asked out loud, so it is
/// graded on a short clock: a household that is being recorded and has said
/// nothing for this long is possible, but a live tier that has written nothing
/// for this long is far more likely to be broken — and it was, twice, on
/// 2026-09-03.
pub fn live_quiet() -> Duration {
    Duration::minutes(20)
}

/// What the recorders delivered in the live tier's own window, and how much of
/// it the speech scanner has actually measured.
///
/// ⚠ **`scanned_s` is separate from `delivered_s` on purpose.** `speech_s` is
/// filled by `audiod speech` on its own cadence, so a window reads "no speech"
/// both when the house was quiet and when nothing in it has been scanned yet.
/// Collapsing those two would silence [`live_check`] exactly when the archive
/// fell behind — which is the condition most likely to accompany a live stall,
/// so the check would go quiet precisely when it was needed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowAudio {
    /// Seconds of audio any device source delivered inside the window.
    pub delivered_s: f64,
    /// Seconds of that audio carrying a `speech_s` measurement.
    pub scanned_s: f64,
    /// Seconds of speech found within the scanned part.
    pub speech_s: f64,
}

/// How much of a window must be scanned before "nobody spoke" is believable.
/// Below this the window is unmeasured, not quiet.
const SCANNED_ENOUGH: f64 = 0.75;

/// Speech in the window that a spurious VAD blip could not account for. A real
/// exchange in a 20-minute window runs to minutes; this floor only keeps a
/// stray second or two from indicting the live tier.
const SPOKE_AT_ALL_S: f64 = 5.0;

impl WindowAudio {
    /// Did the household audibly say something live should have transcribed?
    ///
    /// `None` when the window cannot answer — nothing scanned, or too little of
    /// it — which callers must treat as "cannot certify quiet", never as quiet.
    fn spoke(self) -> Option<bool> {
        if self.delivered_s <= 0.0 {
            return Some(false);
        }
        if self.scanned_s < self.delivered_s * SCANNED_ENOUGH {
            return None;
        }
        Some(self.speech_s >= SPOKE_AT_ALL_S)
    }
}

/// Is live transcription still PRODUCING, or has it merely stayed alive?
///
/// ⚠ **Process existence proved nothing here**, which is the whole reason for
/// the check. On 2026-09-03 live's consumer thread died on an exception while
/// its reader thread carried on consuming the mic at ~10% CPU: the agent was
/// up, `KeepAlive` was satisfied, capture was writing files, agents were
/// loaded, the archive was mirroring — every existing check green — and the
/// tier that exists to answer "what did they just say" wrote nothing for 40
/// minutes, then 11 more after a restart. It was found because a person asked
/// and the system could not answer, which is not a monitoring strategy.
///
/// So this reads the OUTPUT, the way [`capture_checks`] reads files on disk
/// rather than asking the recorder how it feels. A deliberate pause skips
/// (nothing is being recorded, so nothing should be transcribed), matching
/// capture's rule.
///
/// ⚠ **`recent` is what stops this check being ignorable.** Measured 2026-09-09
/// over 55.8 active hours: it was red for 36% of them, and 4.7 of those 20.2
/// red hours were simply a quiet house. A check that blames the tier for the
/// household's silence teaches a person to stop reading it, and the 15.5 h that
/// remain are real (#1383). So silence SKIPS — but only when the window was
/// measured well enough to say so; see [`WindowAudio`].
pub fn live_check(
    newest_turn: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    paused_until: Option<DateTime<Utc>>,
    quiet: Duration,
    recent: WindowAudio,
) -> Check {
    let expected = format!(
        "a live turn within {:.0} min",
        quiet.num_seconds() as f64 / 60.0
    );
    if let Some(until) = paused_until
        && until > now
    {
        return check(
            "capture",
            "live transcription",
            Verdict::Skip,
            format!("paused until {}", to_the_minute(until)),
            expected,
        )
        .build();
    }
    // Nothing was said, and the window is measured well enough to know it.
    if recent.spoke() == Some(false) {
        let why = if recent.delivered_s <= 0.0 {
            // Capture's own checks grade a recorder that delivered nothing;
            // failing here too would count one outage twice.
            "no audio delivered in the window".to_owned()
        } else {
            format!(
                "no speech in the last {:.0} min ({:.0} min of audio scanned)",
                quiet.num_seconds() as f64 / 60.0,
                recent.scanned_s / 60.0,
            )
        };
        return check(
            "capture",
            "live transcription",
            Verdict::Skip,
            why,
            expected,
        )
        .build();
    }
    let Some(newest) = newest_turn else {
        return check(
            "capture",
            "live transcription",
            Verdict::Fail,
            "live has never produced a turn",
            expected,
        )
        .build();
    };
    let since = now - newest;
    check(
        "capture",
        "live transcription",
        if since >= quiet {
            Verdict::Fail
        } else {
            Verdict::Pass
        },
        format!("newest live turn {:.0} min ago", minutes(since)),
        expected,
    )
    .trend(minutes(since), "min")
    .build()
}

/// One pass of the worker, stamped when it began and again when it ended.
///
/// `finished` is `None` while the pass is still running — the distinction
/// between "the loop has stopped starting passes" and "a pass has stopped
/// returning", which point at launchd and at the archive respectively.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct Beat {
    #[serde(deserialize_with = "crate::instant::de")]
    pub started: DateTime<Utc>,
    #[serde(default, deserialize_with = "crate::instant::de_opt")]
    pub finished: Option<DateTime<Utc>>,
    #[serde(default)]
    pub seconds: Option<f64>,
    #[serde(default)]
    pub rows: i64,
}

// How long the worker may go without completing a pass. Both ends measured on
// the Mac, 2026-08-10, straight after a home-manager switch:
//
//     first pass after a restart   513 s   (8.5 min, 0 rows)
//     steady-state empty pass       17-22 s
//
// ⚠ **An empty pass is not a fast pass**, which is the thing to know before
// touching these numbers. A pass that writes no transcript rows still runs the
// bounded backfills — loudness, word timings, speaker ID — and the first one
// after a restart loads the pyannote embedding models off the spinning disk,
// which is where 8.5 of those minutes go. So the warn line is set from the
// COLD start, not the steady state: 30 min is three and a half times the
// slowest real pass ever measured here. Tighter than that and every activation
// paints the pipeline yellow, which is how a colour stops meaning anything.
//
// The fail line is the incident's own shape: the 2026-08-10 starvation ran
// over an hour, and no legitimate pass has come close to that.
pub fn worker_slow() -> Duration {
    Duration::minutes(30)
}
pub fn worker_stopped() -> Duration {
    Duration::hours(1)
}

/// Is the transcription pipeline still turning, or has it merely gone quiet?
///
/// ⚠ **The worker's log cannot answer this**, which is the whole reason for
/// the check: it prints only when a pass writes transcript rows, so a house
/// with nothing to transcribe and a worker in uninterruptible disk wait
/// produce the same empty log. On 2026-08-10 that log was three days old and
/// an hour of captured audio was going unindexed behind it (#709).
///
/// A running pass and a stopped loop are both graded on the same clock — time
/// since a pass last *completed* — but they are named differently in
/// `observed`, because they point at different things: a pass that will not
/// return is the archive, and a loop that will not start one is launchd.
pub fn worker_check(
    beat: Option<&Beat>,
    now: DateTime<Utc>,
    slow: Duration,
    stopped: Duration,
) -> Check {
    let expected = format!(
        "a pass completed within {:.0} min",
        slow.num_seconds() as f64 / 60.0
    );
    let Some(beat) = beat else {
        return check(
            "capture",
            "worker pulse",
            Verdict::Fail,
            "the worker has never completed a pass",
            expected,
        )
        .build();
    };

    let reference = beat.finished.unwrap_or(beat.started);
    let since = now - reference;
    let observed = if beat.finished.is_none() {
        format!("a pass has been running {:.1} min", minutes(since))
    } else {
        let rows = if beat.rows == 0 {
            "nothing to do".to_owned()
        } else {
            format!("{} row(s)", beat.rows)
        };
        let took = beat
            .seconds
            .map_or_else(String::new, |s| format!(" in {s:.1}s"));
        format!("last pass {:.1} min ago — {rows}{took}", minutes(since))
    };

    let verdict = if since >= stopped {
        Verdict::Fail
    } else if since >= slow {
        Verdict::Warn
    } else {
        Verdict::Pass
    };
    check("capture", "worker pulse", verdict, observed, expected)
        .trend(minutes(since), "min")
        .build()
}

/// One check per launchd agent. An installed-but-unloaded agent is always a
/// fault: the agents self-gate (they park while capture is paused, they do not
/// unload), so "not loaded" never means "deliberately off".
pub fn agent_checks(agents: &[(String, bool)]) -> Vec<Check> {
    if agents.is_empty() {
        return vec![
            check(
                "agents",
                "installed",
                Verdict::Fail,
                "no recall agents installed",
                "every agent loaded",
            )
            .build(),
        ];
    }
    let mut sorted: Vec<&(String, bool)> = agents.iter().collect();
    sorted.sort();
    sorted
        .into_iter()
        .map(|(label, loaded)| {
            check(
                "agents",
                label.clone(),
                if *loaded {
                    Verdict::Pass
                } else {
                    Verdict::Fail
                },
                if *loaded { "loaded" } else { "NOT LOADED" },
                "loaded",
            )
            .build()
        })
        .collect()
}
