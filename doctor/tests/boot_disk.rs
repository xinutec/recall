//! launchd and the pause file. The pause file is on the archive volume, so the
//! reporting process learns it from the child.

use doctor::agents::{PAUSE_FILE, agent_health, paused_until};

#[test]
fn the_child_reports_the_pause_so_the_parent_need_not_read_the_volume() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(PAUSE_FILE),
        "2026-09-08T19:11:22.164504+00:00\n",
    )
    .unwrap();
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
        collected["paused_until"], "2026-09-08T19:11:22.164504+00:00",
        "{collected}"
    );
}

#[test]
fn no_pause_file_is_not_paused() {
    let dir = tempfile::tempdir().unwrap();
    assert!(paused_until(dir.path()).is_none());
}

#[test]
fn a_pause_file_reads_back_as_its_instant() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(PAUSE_FILE),
        "2026-09-08T19:11:22.164504+00:00\n",
    )
    .unwrap();
    assert_eq!(paused_until(dir.path()).unwrap().timestamp(), 1_788_894_682);
}

#[test]
fn a_naive_pause_timestamp_is_read_as_utc_rather_than_refused() {
    // Refusing it would read as not paused.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(PAUSE_FILE), "2026-09-08T19:11:22").unwrap();
    assert_eq!(paused_until(dir.path()).unwrap().timestamp(), 1_788_894_682);
}

#[test]
fn an_unparseable_pause_file_is_not_paused() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(PAUSE_FILE), "soon").unwrap();
    assert!(paused_until(dir.path()).is_none());
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
