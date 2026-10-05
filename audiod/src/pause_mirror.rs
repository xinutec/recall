//! Mirror the household pause onto a recorder.
//!
//! - `poll` (the Linux recorder) reads `/api/capture` and maintains the
//!   `capture_paused_until` file every capture loop gates on.
//! - `exchange` (the Mac) reports what it applied, with each source's `.alive`
//!   freshness, and long-polls for the intent, in one round trip.
//!
//! Edge-triggered: intent is applied only when it changes (a local marker), or
//! an unchanged "running" would clobber a pause pressed on the Mac's own UI.
//! An unreachable fleet leaves the last state standing: inventing a pause on
//! error would silence a recorder over a wifi blip. A stale pause expires by
//! its bounded timestamp.

use crate::pause::{PAUSE_FILE, paused_until};
use chrono::{DateTime, SecondsFormat, Utc};
use std::path::Path;
use std::time::Duration;

const POLL: Duration = Duration::from_secs(5);

#[derive(Debug, PartialEq, Eq)]
pub enum Desired {
    Running,
    PausedUntil(String),
}

fn fetch(agent: &ureq::Agent, url: &str) -> Result<Desired, String> {
    let text = agent
        .get(&format!("{url}/api/capture"))
        .call()
        .map_err(|e| e.to_string())?
        .into_string()
        .map_err(|e| e.to_string())?;
    let body: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    // The fleet reports desired state alongside applied; a recorder obeys
    // desired.
    let running = body["desiredRunning"]
        .as_bool()
        .or_else(|| body["running"].as_bool())
        .ok_or("no running field")?;
    if running {
        return Ok(Desired::Running);
    }
    let until = body["desiredPausedUntil"]
        .as_str()
        .or_else(|| body["pausedUntil"].as_str())
        .ok_or("paused with no bound")?;
    Ok(Desired::PausedUntil(until.to_owned()))
}

fn apply(root: &Path, desired: &Desired) {
    let path = root.join(PAUSE_FILE);
    match desired {
        Desired::Running => {
            if path.exists() {
                match std::fs::remove_file(&path) {
                    Ok(()) => tracing::info!("pause-mirror: resumed"),
                    Err(err) => tracing::warn!(%err, "pause-mirror: cannot clear pause"),
                }
            }
        }
        Desired::PausedUntil(until) => {
            let current = std::fs::read_to_string(&path).ok();
            if current.as_deref().map(str::trim) != Some(until.as_str()) {
                match std::fs::write(&path, until) {
                    Ok(()) => tracing::info!(%until, "pause-mirror: paused"),
                    Err(err) => tracing::warn!(%err, "pause-mirror: cannot write pause"),
                }
            }
        }
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
            Ok(desired) => apply(root, &desired),
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
    /// Apply this, then record it as applied.
    Apply { desired: Desired, record: String },
}

/// An unparseable or elapsed intent resolves to Running (an expired pause is
/// not a pause), and is recorded, so it is not retried every tick.
#[must_use]
pub fn decide(applied: &str, intent: Option<&str>, now: DateTime<Utc>) -> Decision {
    let record = intent.unwrap_or("").to_owned();
    if record == applied {
        return Decision::Unchanged;
    }
    let desired = match intent.filter(|i| !i.is_empty()) {
        None => Desired::Running,
        Some(raw) => match DateTime::parse_from_rfc3339(raw) {
            Ok(until) if until.with_timezone(&Utc) > now => Desired::PausedUntil(raw.to_owned()),
            Ok(_) => Desired::Running,
            Err(_) => {
                tracing::warn!(intent = %raw, "unparseable fleet intent — treating as running");
                Desired::Running
            }
        },
    };
    Decision::Apply { desired, record }
}

/// Each source's last proved recording time: its `.alive` marker's mtime. The
/// fleet cannot see these files, so every pass ships them.
#[must_use]
pub fn source_liveness(root: &Path) -> serde_json::Map<String, serde_json::Value> {
    let mut out = serde_json::Map::new();
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
        out.insert(
            name,
            serde_json::Value::String(stamp.to_rfc3339_opts(SecondsFormat::Micros, false)),
        );
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
) -> Result<Option<String>, String> {
    let local = paused_until(root);
    let body = serde_json::json!({
        "running": local.is_none_or(|until| until <= now),
        "pausedUntil": local.map(|u| u.to_rfc3339_opts(SecondsFormat::Micros, false)),
        "sourceLiveness": source_liveness(root),
        "wait": wait,
        "knownIntent": if applied.is_empty() { None } else { Some(applied) },
    });
    // ureq is built without `json`.
    let text = agent
        .post(&format!("{url}/sync/capture"))
        .set("authorization", &format!("Bearer {token}"))
        .set("content-type", "application/json")
        .send_string(&body.to_string())
        .map_err(|e| e.to_string())?
        .into_string()
        .map_err(|e| e.to_string())?;
    let parsed: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(parsed
        .get("pausedUntil")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned))
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
            Ok(intent) => match decide(&applied, intent.as_deref(), Utc::now()) {
                Decision::Unchanged => false,
                Decision::Apply { desired, record } => {
                    apply(root, &desired);
                    // Recorded even when unparseable; see `decide`.
                    if let Err(err) = std::fs::write(root.join(MARKER_FILE), &record) {
                        tracing::warn!(%err, "pause-mirror: cannot record applied intent");
                    }
                    tracing::info!(
                        intent = if record.is_empty() {
                            "running"
                        } else {
                            record.as_str()
                        },
                        "pause-mirror: fleet intent changed — applied"
                    );
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
pub fn exchange_once(
    root: &Path,
    url: &str,
    token: &str,
    _interval: Duration,
) -> std::process::ExitCode {
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
            let shown = intent.as_deref().unwrap_or("(running)");
            match decide(&applied, intent.as_deref(), Utc::now()) {
                Decision::Unchanged => {
                    println!("fleet intent {shown} — unchanged since last applied; nothing done");
                }
                Decision::Apply { desired, record } => {
                    apply(root, &desired);
                    if let Err(err) = std::fs::write(root.join(MARKER_FILE), &record) {
                        eprintln!("capture-mirror: cannot record applied intent: {err}");
                        return std::process::ExitCode::FAILURE;
                    }
                    println!("fleet intent {shown} — applied {desired:?}");
                }
            }
            std::process::ExitCode::SUCCESS
        }
    }
}
