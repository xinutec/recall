//! Mirror the household pause onto a recorder that has no local control plane
//! (stage C3: geb). Polls Isis's login-free `/api/capture` and maintains the
//! same `capture_paused_until` file every capture loop already self-gates on.
//!
//! Conservative in the one direction that matters for a recorder: the mirror
//! only ever writes a pause the control plane EXPLICITLY stated (a bounded
//! `pausedUntil`), and an unreachable control plane leaves the last state
//! standing — a poller that invented a pause on error would be a recorder
//! silenced by a wifi blip, and completeness outranks (design.md §1). The
//! pause file's own bounded timestamp is the backstop: even a stale pause
//! expires by itself.

use crate::pause::PAUSE_FILE;
use std::path::Path;
use std::time::Duration;

const POLL: Duration = Duration::from_secs(5);

#[derive(Debug, PartialEq, Eq)]
enum Desired {
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
    // DESIRED — it is the control plane's word, which is the whole point.
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

/// Poll forever. Never exits — a mirror that died would freeze the last
/// state, so systemd owns the restart.
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
