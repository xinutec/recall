//! launchd, and the pause file — the two things the reporting process may read.
//!
//! Both live on the boot disk. Nothing here touches the archive volume: that is
//! the whole boundary this crate is built around (see [`crate::bounded`]).

use chrono::{DateTime, Utc};
use std::collections::BTreeSet;
use std::path::Path;

const AGENT_PREFIX: &str = "org.xinutec.recall-";

/// The pause file every capture agent self-gates on.
pub const PAUSE_FILE: &str = "capture_paused_until";

/// Recall agent labels currently loaded in launchd.
///
/// Empty when `launchctl` is not there — e.g. a Linux container: capture runs
/// on the Mac, so "no agents loaded" is the right answer, not a crash.
fn loaded_agents() -> BTreeSet<String> {
    let Ok(out) = std::process::Command::new("launchctl").arg("list").output() else {
        return BTreeSet::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().last())
        .filter(|label| label.starts_with(AGENT_PREFIX))
        .map(ToOwned::to_owned)
        .collect()
}

/// Labels of every installed recall agent (its plist in `~/Library/LaunchAgents`).
fn installed_agents(home: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(home.join("Library").join("LaunchAgents")) else {
        return Vec::new();
    };
    let mut labels: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let label = name.strip_suffix(".plist")?;
            label.starts_with(AGENT_PREFIX).then(|| label.to_owned())
        })
        .collect();
    labels.sort();
    labels
}

/// Every installed agent and whether launchd has it loaded. Self-gating means
/// agents stay loaded even while paused, so installed-but-not-loaded is a fault.
pub fn agent_health(home: &Path) -> Vec<(String, bool)> {
    let loaded = loaded_agents();
    installed_agents(home)
        .into_iter()
        .map(|label| {
            let up = loaded.contains(&label);
            (label, up)
        })
        .collect()
}

/// The recorded resume-by time, or `None` if not paused.
///
/// A hand-written naive timestamp is read as UTC rather than refused: this
/// gates every capture agent's main loop, and refusing to parse would read as
/// "not paused" in the one direction that silences a household's control.
pub fn paused_until(root: &Path) -> Option<DateTime<Utc>> {
    let text = std::fs::read_to_string(root.join(PAUSE_FILE)).ok()?;
    crate::instant::parse(text.trim())
}
