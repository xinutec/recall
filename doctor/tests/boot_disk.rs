//! launchd and the capture gate. The gate file is on the archive volume, so
//! the reporting process learns it from the child.

use audiocore::capture::{GATE_FILE, Window};
use doctor::agents::{agent_health, gate};

#[test]
fn the_child_reports_the_gate_so_the_parent_need_not_read_the_volume() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(GATE_FILE), "hold 2026-09-08T19:11:22Z\n").unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_doctor"))
        .arg("--out")
        .arg(dir.path())
        .arg("--collect")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let collected: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        collected["window"], "hold 2026-09-08T19:11:22Z",
        "{collected}"
    );
}

#[test]
fn a_gate_file_reads_back_as_its_window() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(GATE_FILE), "off\n").unwrap();
    assert_eq!(gate(dir.path()), Some(Window::Off));
}

#[test]
fn no_gate_file_or_an_unreadable_one_is_a_fault() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(gate(dir.path()), None);
    std::fs::write(dir.path().join(GATE_FILE), "soon").unwrap();
    assert_eq!(gate(dir.path()), None);
}

#[test]
fn only_recall_plists_count_as_installed_agents() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("Library").join("LaunchAgents");
    std::fs::create_dir_all(&dir).unwrap();
    for name in [
        "org.xinutec.recall-worker.plist",
        "org.xinutec.recall-capture.plist",
        "com.apple.something.plist",
        "org.xinutec.recall-notes.txt",
    ] {
        std::fs::write(dir.join(name), "").unwrap();
    }
    let labels: Vec<String> = agent_health(home.path())
        .into_iter()
        .map(|(label, _)| label)
        .collect();
    assert_eq!(
        labels,
        vec![
            "org.xinutec.recall-capture".to_owned(),
            "org.xinutec.recall-worker".to_owned()
        ]
    );
}

#[test]
fn a_machine_with_no_launchagents_directory_reports_none() {
    // As in a Linux container.
    let home = tempfile::tempdir().unwrap();
    assert!(agent_health(home.path()).is_empty());
}
