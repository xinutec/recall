//! Bounding the agents' logs (#1656).

use audiod::logrotate::{Pass, run};
use std::io::Write;

fn write_log(dir: &std::path::Path, name: &str, lines: usize) -> std::path::PathBuf {
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).expect("create");
    for i in 0..lines {
        writeln!(f, "line {i} {}", "x".repeat(90)).expect("write");
    }
    path
}

#[test]
fn an_oversized_log_is_truncated_and_its_tail_kept() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = write_log(dir.path(), "capture.err.log", 4_000);
    let before = std::fs::metadata(&path).expect("meta").len();
    let cap = before / 4;

    let pass = run(dir.path(), cap).expect("rotate");
    assert_eq!(pass.rotated, 1);

    let after = std::fs::metadata(&path).expect("meta").len();
    assert_eq!(after, 0, "the live log is truncated, not deleted");
    let kept = std::fs::read_to_string(dir.path().join("capture.err.log.1")).expect("tail");
    assert!(kept.len() as u64 <= cap);
    assert!(
        kept.starts_with("line "),
        "the tail must begin at a line break, not mid-line: {:?}",
        &kept[..20.min(kept.len())]
    );
    // The tail is the END of the log — that is what a reader wants after a crash.
    assert!(kept.trim_end().ends_with(&"x".repeat(90)));
    assert!(kept.contains("line 3999"));
}

#[test]
fn a_log_under_the_cap_is_left_alone() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = write_log(dir.path(), "speech.out.log", 10);
    let before = std::fs::read_to_string(&path).expect("read");

    let pass = run(dir.path(), 1024 * 1024).expect("rotate");
    assert_eq!(pass.examined, 1);
    assert_eq!(pass.rotated, 0);
    assert_eq!(std::fs::read_to_string(&path).expect("read"), before);
    assert!(!dir.path().join("speech.out.log.1").exists());
}

#[test]
fn the_inode_survives_so_a_running_agent_keeps_writing() {
    // ⚠ The reason this is copytruncate and not a rename: launchd opened the
    // agent's stdout before any code ran, and it holds the FD. Replacing the
    // file would leave the agent writing to an inode nothing can read.
    let dir = tempfile::tempdir().expect("tmp");
    let path = write_log(dir.path(), "runner.err.log", 4_000);
    let mut held = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("open as the agent does");

    run(dir.path(), 1024).expect("rotate");
    writeln!(held, "after rotation").expect("the agent keeps writing");

    let live = std::fs::read_to_string(&path).expect("read");
    assert!(
        live.contains("after rotation"),
        "a line written after rotation must land in the live log: {live:?}"
    );
}

#[test]
fn a_directory_of_other_files_is_not_touched() {
    let dir = tempfile::tempdir().expect("tmp");
    std::fs::write(dir.path().join("recall.sqlite"), vec![0u8; 4096]).expect("db");
    std::fs::write(dir.path().join("notes.txt"), vec![0u8; 4096]).expect("txt");
    assert_eq!(run(dir.path(), 16).expect("rotate"), Pass::default());
    assert!(dir.path().join("recall.sqlite").exists());
}
