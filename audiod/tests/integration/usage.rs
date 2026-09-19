//! `--help` must name every mode the dispatcher accepts.

/// ⚠ Five of ten modes were missing when this was written — including `pause`
/// and `resume`, the break-glass control — so the one surface a person reaches
/// for when the fleet is unreachable did not mention it (#1395).
///
/// Reads the binary's source rather than a second list: a list that can
/// disagree with the dispatcher is the defect being guarded against.
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
