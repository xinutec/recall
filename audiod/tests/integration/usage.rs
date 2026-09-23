//! `--help` must name every mode the dispatcher accepts.

/// `--help` is what a person reads when the fleet is unreachable, so it must
/// list break-glass modes like `pause`. Reads the binary's source rather than
/// a second list, since a list can disagree with the dispatcher.
#[test]
fn every_mode_the_dispatcher_accepts_is_in_the_usage_text() {
    let source = include_str!("../../src/main.rs");
    let modes: Vec<&str> = source
        .match_indices("Some(\"")
        .filter_map(|(at, _)| source[at + 6..].split('"').next())
        .collect();
    assert!(modes.len() >= 8, "found only {modes:?}");
    let usage = source
        .split("usage: ")
        .nth(1)
        .expect("the usage string")
        .split("ExitCode::FAILURE")
        .next()
        .expect("its end");
    for mode in modes {
        assert!(
            usage.contains(&format!("audiod {mode} "))
                || usage.contains(&format!("audiod {mode}\\n")),
            "`{mode}` is dispatched but absent from --help"
        );
    }
}
