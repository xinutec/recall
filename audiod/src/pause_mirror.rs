//! Mirror the capture window onto a recorder's gate file.
//!
//! - `poll` (the Linux recorder) reads `/api/capture` and maintains the
//!   [`GATE_FILE`] every capture loop checks.
//! - `exchange` (the Mac) reports what it applied, with each source's `.alive`
//!   freshness, and long-polls for the intent, in one round trip.
//!
//! Edge-triggered: intent is applied only when it changes (a local marker), or
//! an unchanged intent would clobber a break-glass change made on the Mac.
//! An unreachable fleet leaves the last window standing, and a window closes
//! at its own end.

use crate::gate::{GATE_FILE, window};
use audiocore::capture::{Applied, CaptureState, Intent, Window};
use chrono::{DateTime, SecondsFormat, Utc};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

const POLL: Duration = Duration::from_secs(5);

fn fetch(agent: &ureq::Agent, url: &str) -> Result<Option<Window>, String> {
    let text = agent
        .get(&format!("{url}/api/capture"))
        .call()
        .map_err(|e| e.to_string())?
        .into_string()
        .map_err(|e| e.to_string())?;
    let state: CaptureState = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    // A recorder obeys the desired half.
    Ok(state.desired_window())
}

/// Write the window, or for a fault (`None`) remove the file, which records.
fn apply(root: &Path, desired: Option<Window>) {
    let Some(desired) = desired else {
        tracing::warn!("pause-mirror: the fleet's window does not read — recording");
        match std::fs::remove_file(root.join(GATE_FILE)) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                tracing::warn!(%err, "pause-mirror: cannot clear the gate");
            }
            _ => {}
        }
        return;
    };
    if window(root) == Some(desired) {
        return;
    }
    match crate::gate::write(root, desired) {
        Ok(()) => tracing::info!(window = %desired.spell(), "pause-mirror: applied"),
        Err(err) => tracing::warn!(%err, "pause-mirror: cannot write the gate"),
    }
}

/// Poll forever; systemd owns the restart.
pub fn run(root: &Path, url: &str) -> ! {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build();
    loop {
        match fetch(&agent, url) {
            Ok(desired) => apply(root, desired),
            Err(err) => {
                tracing::debug!(%err, "pause-mirror: unreachable; keeping last state");
            }
        }
        std::thread::sleep(POLL);
    }
}

/// Records the last fleet intent this mirror applied, so an unchanged pass is a
/// no-op.
pub const MARKER_FILE: &str = "capture_intent_mirrored";

/// What a pass should do, from the marker and the fleet's answer alone.
#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    /// The fleet's intent is what we last applied. Leave the local file alone.
    Unchanged,
    /// Apply this (`None`: a fault), then record it as applied.
    Apply {
        window: Option<Window>,
        record: String,
    },
}

/// An unparseable intent is a fault, which records, and is recorded, so it is
/// not retried every tick.
#[must_use]
pub fn decide(applied: &str, intent: &str) -> Decision {
    if intent == applied {
        return Decision::Unchanged;
    }
    Decision::Apply {
        window: Window::parse(intent),
        record: intent.to_owned(),
    }
}

/// Each source's last proved recording time: its `.alive` marker's mtime. The
/// fleet cannot see these files, so every pass ships them.
#[must_use]
pub fn source_liveness(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let marker = entry.path().join(".alive");
        let Ok(meta) = std::fs::metadata(&marker) else {
            continue;
        };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        let Some(name) = entry.file_name().to_str().map(ToOwned::to_owned) else {
            continue;
        };
        let stamp: DateTime<Utc> = modified.into();
        out.insert(name, stamp.to_rfc3339_opts(SecondsFormat::Micros, false));
    }
    out
}

/// One exchange: report what this Mac applied, receive the fleet's intent.
///
/// `wait` asks the fleet to hold the reply while its intent equals `applied`,
/// so a press anywhere comes back in about a round trip.
///
/// # Errors
/// Transport or protocol failure; the caller keeps the last state on either.
pub fn exchange(
    agent: &ureq::Agent,
    url: &str,
    token: &str,
    root: &Path,
    applied: &str,
    wait: f64,
    now: DateTime<Utc>,
) -> Result<String, String> {
    let local = window(root);
    let body = Applied {
        running: local.is_none_or(|w| w.records(now)),
        window: local.map(Window::spell),
        source_liveness: source_liveness(root),
        wait,
        known_intent: (!applied.is_empty()).then(|| applied.to_owned()),
    };
    // ureq is built without `json`.
    let text = agent
        .post(&format!("{url}/sync/capture"))
        .set("authorization", &format!("Bearer {token}"))
        .set("content-type", "application/json")
        .send_string(&serde_json::to_string(&body).expect("a report serialises"))
        .map_err(|e| e.to_string())?
        .into_string()
        .map_err(|e| e.to_string())?;
    let intent: Intent = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(intent.window)
}

fn read_marker(root: &Path) -> String {
    std::fs::read_to_string(root.join(MARKER_FILE))
        .map(|s| s.trim().to_owned())
        .unwrap_or_default()
}

/// Mirror the fleet's intent forever, reporting each pass. A failure is logged
/// and the loop goes on.
pub fn run_exchange(root: &Path, url: &str, token: &str, interval: Duration) -> ! {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        // Longer than the fleet's hold.
        .timeout(interval + Duration::from_secs(20))
        .build();
    loop {
        let started = std::time::Instant::now();
        let applied = read_marker(root);
        let changed = match exchange(
            &agent,
            url,
            token,
            root,
            &applied,
            interval.as_secs_f64(),
            Utc::now(),
        ) {
            Ok(intent) => match decide(&applied, &intent) {
                Decision::Unchanged => false,
                Decision::Apply { window, record } => {
                    apply(root, window);
                    // Recorded even when unparseable; see `decide`.
                    if let Err(err) = std::fs::write(root.join(MARKER_FILE), &record) {
                        tracing::warn!(%err, "pause-mirror: cannot record applied intent");
                    }
                    tracing::info!(intent = %record, "pause-mirror: fleet intent changed — applied");
                    true
                }
            },
            Err(err) => {
                tracing::debug!(%err, "pause-mirror: exchange failed; keeping last state");
                false
            }
        };
        // A change is reported at once (settling "Pausing…"); otherwise sleep
        // what the long poll has not already spent.
        if !changed {
            // A pass that outran the interval sleeps zero.
            std::thread::sleep(interval.saturating_sub(started.elapsed()));
        }
    }
}

/// One exchange, applied, then exit: for checking a real fleet by hand without
/// a second mirror competing for the pause file. Prints what it saw and did.
pub fn exchange_once(root: &Path, url: &str, token: &str) -> std::process::ExitCode {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout(Duration::from_secs(20))
        .build();
    let applied = read_marker(root);
    // wait = 0: a one-shot must not hang on the fleet's long-poll.
    match exchange(&agent, url, token, root, &applied, 0.0, Utc::now()) {
        Err(err) => {
            eprintln!("capture-mirror: exchange failed: {err}");
            std::process::ExitCode::FAILURE
        }
        Ok(intent) => {
            let shown = &intent;
            match decide(&applied, &intent) {
                Decision::Unchanged => {
                    println!("fleet intent {shown} — unchanged since last applied; nothing done");
                }
                Decision::Apply { window, record } => {
                    apply(root, window);
                    if let Err(err) = std::fs::write(root.join(MARKER_FILE), &record) {
                        eprintln!("capture-mirror: cannot record applied intent: {err}");
                        return std::process::ExitCode::FAILURE;
                    }
                    println!("fleet intent {shown} — applied {window:?}");
                }
            }
            std::process::ExitCode::SUCCESS
        }
    }
}
