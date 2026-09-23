//! Is the recording actually recording: is audio still landing on disk?
//!
//! launchd restarts capture when it dies (`KeepAlive`), so a persistent fault
//! becomes a crash loop, which from outside looks exactly like a quiet house.
//! This reads the filesystem, not the transcription pipeline: a segment file
//! appears every 60 seconds while capture lives, and the pipeline can be hours
//! behind without the microphone having stopped.
//!
//! The verdicts differ by recorder:
//!
//! * the **always-on mic** (wired to this machine) has no excuse for silence:
//!   `fail`.
//! * a **phone** leaves the house, runs flat, has its app closed: `warn`.
//! * **every source silent at once** is the capture process or the machine:
//!   `fail`.
//!
//! A paused recording reports `skip` with the resume time.

use crate::check::{Check, Verdict, check};
use crate::source::SourceKind;
use chrono::{DateTime, Duration, Utc};
use std::path::Path;

/// A live capture writes a segment every 60 seconds. Two missed rotations is
/// noise (a slow disk, a rotation straddling the check); five is not.
pub fn silent_after() -> Duration {
    Duration::minutes(5)
}

/// The always-on mic: wired to the recording machine, so never excused silence.
pub const ALWAYS_ON: SourceKind = SourceKind::CoreAudio;

/// One microphone, and when audio last landed on disk from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorder {
    pub source_id: String,
    pub kind: SourceKind,
    pub last_audio: Option<DateTime<Utc>>,
}

/// Minutes, to one decimal: the unit every duration in a check is reported in.
pub fn minutes(since: Duration) -> f64 {
    (since.num_milliseconds() as f64 / 60_000.0 * 10.0).round() / 10.0
}

/// How a pause is spelled in `observed`: `datetime.isoformat(timespec="minutes")`.
fn to_the_minute(when: DateTime<Utc>) -> String {
    when.format("%Y-%m-%dT%H:%M%:z").to_string()
}

/// What fleetwatch should be told about the recording, right now.
///
/// Pure: the filesystem read happens in [`recorders_on_disk`], so the rules are
/// testable without a microphone.
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
        // Deliberate, so not a fault, but shown: a forgotten pause loses days.
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

    // The summary: every microphone silent together is the capture process or
    // the machine it runs on, not coincidence.
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

/// When each microphone last wrote audio: the newest non-empty segment's mtime
/// in its archive directory.
///
/// Zero-byte segments are skipped: capture can roll a fresh file every segment
/// while the device delivers nothing (a coreaudio startup dead window), and
/// those stubs must not read as recording.
pub fn recorders_on_disk(root: &Path, sources: &[(String, SourceKind)]) -> Vec<Recorder> {
    sources
        .iter()
        .map(|(source_id, kind)| {
            let mut newest: Option<std::time::SystemTime> = None;
            // Segments are `<source_id>-*`; the mtime is compared, so the
            // directory is read in any order.
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

/// How long the live tier may go without a turn. Short, because it answers a
/// question asked out loud; a quiet window is excused by [`WindowAudio`].
pub fn live_quiet() -> Duration {
    Duration::minutes(20)
}

/// What the recorders delivered in the live tier's window, and how much of it
/// the fleet has measured for speech.
///
/// ⚠ `scanned_s` is separate from `delivered_s` on purpose. Speech is measured
/// on its own cadence, so "no speech" can mean a quiet house or an unscanned
/// window; collapsing the two would silence [`live_check`] when the
/// measurement falls behind, which is when a live stall is most likely.
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
/// exchange in a 20-minute window runs to minutes; this only keeps a stray
/// second or two from indicting the live tier.
const SPOKE_AT_ALL_S: f64 = 5.0;

impl WindowAudio {
    /// Did the household audibly say something live should have transcribed?
    ///
    /// `None` when too little of the window was scanned: "cannot certify
    /// quiet", never quiet.
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

/// The lag at which the instant feed warns. A call costs its 30-second Whisper
/// window whatever it holds, so a healthy floor is a few seconds; a regressed
/// feed measured a median of 83.8 s, so 30 s separates the two.
#[must_use]
pub fn live_lag_slow() -> Duration {
    Duration::seconds(30)
}

/// How far back the lag median looks.
///
/// Not the 48-hour loss window: this asks whether the feed keeps up now, and a
/// two-day median blends a fault with its repair. Six hours is longer than any
/// one conversation and shorter than a day.
#[must_use]
pub fn live_lag_window() -> Duration {
    Duration::hours(6)
}

/// How far behind the speaker the instant feed is running.
///
/// The failure [`live_check`] cannot see: turns arriving steadily, each later
/// than the last, as happens when a call costs more than the speech it carries.
///
/// No median skips rather than passing: "nothing to measure" is not "measured
/// and fine".
pub fn live_lag_check(median_seconds: Option<f64>, slow: Duration, unmeasured: &str) -> Check {
    let bound = slow.num_seconds() as f64;
    let expected = format!("live turns arriving within {bound:.0}s of being said");
    let Some(median) = median_seconds else {
        // The caller says why, because only it knows. A skip whose reason is
        // wrong is how a blind check gets trusted.
        return check(
            "capture",
            "live delivery lag",
            Verdict::Skip,
            unmeasured.to_owned(),
            expected,
        )
        .build();
    };
    check(
        "capture",
        "live delivery lag",
        if median >= bound {
            Verdict::Warn
        } else {
            Verdict::Pass
        },
        if median >= bound {
            format!(
                "median {median:.1}s behind — the feed is falling further behind as people talk"
            )
        } else {
            format!("median {median:.1}s behind")
        },
        expected,
    )
    .trend((median * 10.0).round() / 10.0, "s")
    .build()
}

/// Is live transcription still producing turns, not merely running? Reads the
/// output, the way [`capture_checks`] reads files on disk.
///
/// A deliberate pause skips. So does a window with no speech, but only when it
/// was measured well enough to say so (see [`WindowAudio`]): a check that blames
/// the tier for a quiet house stops being read.
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

/// One unit of transcription work, as the runner stamps it after each job (an
/// empty queue stamps too, with no rows).
///
/// `finished` is optional in the format: without it the beat reads as a pass
/// still running. The runner always writes it.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct Beat {
    #[serde(deserialize_with = "audiocore::instant::de")]
    pub started: DateTime<Utc>,
    #[serde(default, deserialize_with = "audiocore::instant::de_opt")]
    pub finished: Option<DateTime<Utc>>,
    #[serde(default)]
    pub seconds: Option<f64>,
    #[serde(default)]
    pub rows: i64,
}

// How long the runner may go without stamping its pulse before the check
// warns, then fails. Loose on purpose: a line every restart's cold start
// crosses stops being read.
pub fn worker_slow() -> Duration {
    Duration::minutes(30)
}
pub fn worker_stopped() -> Duration {
    Duration::hours(1)
}

/// Is the transcription pipeline still turning, or has it merely gone quiet?
///
/// Graded on time since the pulse, which the runner stamps even when the queue
/// is empty, so a stale pulse means the runner stopped rather than ran out of
/// work. A beat with no `finished` is reported as a pass still running.
pub fn worker_check(
    beat: Option<&Beat>,
    now: DateTime<Utc>,
    slow: Duration,
    stopped: Duration,
) -> Check {
    let expected = format!(
        "a transcription pass completed within {:.0} min",
        slow.num_seconds() as f64 / 60.0
    );
    let Some(beat) = beat else {
        return check(
            "capture",
            "transcription pulse",
            Verdict::Fail,
            "no transcription pass has ever completed here",
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
    check(
        "capture",
        "transcription pulse",
        verdict,
        observed,
        expected,
    )
    .trend(minutes(since), "min")
    .build()
}

/// One check per launchd agent. The agents self-gate (they park while capture
/// is paused rather than unload), so installed but not loaded is a fault.
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
