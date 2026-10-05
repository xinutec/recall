//! `--help` must name every mode, the break-glass ones above all.

/// `--help` is what a person reads when the fleet is unreachable: it lists
/// `pause` and `resume`.
#[test]
fn every_mode_is_in_the_help() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_audiod"))
        .arg("--help")
        .output()
        .expect("audiod runs");
    assert!(out.status.success(), "{out:?}");
    let help = String::from_utf8_lossy(&out.stdout);
    for mode in [
        "ingest",
        "capture",
        "capture-mirror",
        "pause-mirror",
        "upload",
        "beat-relay",
        "logrotate",
        "pause",
        "resume",
    ] {
        assert!(
            help.lines()
                .any(|l| l.trim_start().starts_with(&format!("{mode} "))),
            "`{mode}` is absent from --help:\n{help}"
        );
    }
}

/// A mode's missing requirement is refused before anything runs.
#[test]
fn a_capture_without_its_source_is_refused() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_audiod"))
        .args(["capture", "--root", "/nonexistent"])
        .output()
        .expect("audiod runs");
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("--id"));
}
