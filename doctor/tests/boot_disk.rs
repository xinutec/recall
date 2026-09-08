//! What the REPORTING process may read: launchd and the pause file. Nothing
//! here touches the archive volume — that is the boundary the crate is built
//! around.

use doctor::agents::{PAUSE_FILE, agent_health, paused_until};

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
    // This gates every capture agent's main loop. Refusing to parse would read
    // as "not paused", which is the one direction that silences a household's
    // control over its own recording.
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
    // Not a crash: the fleet's Linux container has no launchctl and no plists,
    // and "no agents" is the right answer there.
    let home = tempfile::tempdir().unwrap();
    assert!(agent_health(home.path()).is_empty());
}
