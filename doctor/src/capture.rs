//! Is audio still landing on disk? A crash-looping capture looks like a quiet
//! house from outside, and transcription can be hours behind a working mic, so
//! this reads segment files, which appear every 60 seconds.
//!
//! A silent always-on mic fails; a silent phone warns (it may be out, flat or
//! closed); every source silent at once fails. A pause skips.

use crate::check::{Check, Verdict, check};
use crate::source::SourceKind;
use chrono::{DateTime, Duration, Utc};
use std::path::Path;

/// Two missed 60-second segments are noise; five are not.
pub fn silent_after() -> Duration {
    Duration::minutes(5)
}

/// The always-on mic, wired to this machine.
pub const ALWAYS_ON: SourceKind = SourceKind::CoreAudio;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorder {
    pub source_id: String,
    pub kind: SourceKind,
    pub last_audio: Option<DateTime<Utc>>,
}

/// Minutes to one decimal, the unit of every duration in a check.
pub fn minutes(since: Duration) -> f64 {
    (since.num_milliseconds() as f64 / 60_000.0 * 10.0).round() / 10.0
}

/// `2026-09-08T19:11+00:00`.
fn to_the_minute(when: DateTime<Utc>) -> String {
    when.format("%Y-%m-%dT%H:%M%:z").to_string()
}

/// One check per recorder, and a summary. Pure; [`recorders_on_disk`] reads
/// the disk.
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
        // Shown, not passed: a forgotten pause loses days.
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
            Verdict::Fail
        } else {
            Verdict::Warn
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

    // Every mic silent together is capture or the machine, not coincidence.
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

/// When each microphone last wrote audio: the newest non-empty segment's
/// mtime. Empty files are skipped: capture rolls them while a device delivers
/// nothing, as in coreaudio's startup window.
pub fn recorders_on_disk(root: &Path, sources: &[(String, SourceKind)]) -> Vec<Recorder> {
    sources
        .iter()
        .map(|(source_id, kind)| {
            let mut newest: Option<std::time::SystemTime> = None;
            if let Ok(entries) = std::fs::read_dir(root.join(source_id)) {
                let prefix = format!("{source_id}-");
                for entry in entries.flatten() {
                    if !entry.file_name().to_string_lossy().starts_with(&prefix) {
                        continue;
                    }
                    let Ok(meta) = entry.metadata() else { continue };
                    if meta.len() == 0 {
                        continue;
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

/// How long the live tier may go without a turn while people talk.
pub fn live_quiet() -> Duration {
    Duration::minutes(20)
}

/// What the recorders delivered in the live window, and how much of it the
/// server has measured for speech.
///
/// Scanned is kept apart from delivered: speech is measured on its own
/// schedule, and an unscanned window must not pass for a quiet one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowAudio {
    pub delivered_s: f64,
    /// Seconds of the delivered audio measured for speech.
    pub scanned_s: f64,
    /// Seconds of speech in the scanned part.
    pub speech_s: f64,
}

/// The share of a window that must be scanned before it can count as quiet.
const SCANNED_ENOUGH: f64 = 0.75;

/// Speech, in seconds, more than a stray detector blip.
const SPOKE_AT_ALL_S: f64 = 5.0;

impl WindowAudio {
    /// Did anyone speak? `None` when too little was scanned to say.
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

/// The median lag at which the instant feed warns. Healthy is a few seconds; a
/// regressed feed measured 83.8.
#[must_use]
pub fn live_lag_slow() -> Duration {
    Duration::seconds(30)
}

/// How far back the lag median looks: whether the feed keeps up now, not
/// blended with a fault already repaired.
#[must_use]
pub fn live_lag_window() -> Duration {
    Duration::hours(6)
}

/// How far behind the speaker the instant feed runs: turns can keep arriving,
/// each later than the last, which [`live_check`] cannot see. No median skips
/// with the caller's `unmeasured` reason.
pub fn live_lag_check(median_seconds: Option<f64>, slow: Duration, unmeasured: &str) -> Check {
    let bound = slow.num_seconds() as f64;
    let expected = format!("live turns arriving within {bound:.0}s of being said");
    let Some(median) = median_seconds else {
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

/// Is live transcription producing turns? A pause skips, and so does a window
/// measured to have no speech: blaming the tier for a quiet house would teach
/// people to ignore the check.
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
    if recent.spoke() == Some(false) {
        let why = if recent.delivered_s <= 0.0 {
            // The capture checks already grade this.
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

/// The runner's pulse, stamped after each job and each empty poll. Without
/// `finished` it reads as a pass still running; the runner always writes it.
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

// How long without a pulse before the check warns, then fails. Loose enough
// that a cold start does not cross it.
pub fn worker_slow() -> Duration {
    Duration::minutes(30)
}
pub fn worker_stopped() -> Duration {
    Duration::hours(1)
}

/// Is the runner turning? The pulse is stamped even with an empty queue, so a
/// stale one means the runner stopped, not that work ran out.
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

/// One check per launchd agent. Agents stay loaded while paused, so installed
/// but not loaded is a fault.
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
