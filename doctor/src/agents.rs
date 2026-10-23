//! launchd's agents, and the capture gate. The gate file is on the archive
//! volume, so only the child process reads it.

use std::collections::BTreeSet;
use std::path::Path;

const AGENT_PREFIX: &str = "org.xinutec.recall-";

/// Recall agent labels loaded in launchd; empty where there is no `launchctl`.
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

/// Recall agent labels with a plist in `~/Library/LaunchAgents`.
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

/// Every installed agent and whether it is loaded. Agents stay loaded while
/// off, so installed but not loaded is a fault.
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

/// The capture gate's window; `None` when the file is missing or does not
/// read, a fault, which records.
pub fn gate(root: &Path) -> Option<audiocore::capture::Window> {
    audiocore::capture::read_gate(root)
}
