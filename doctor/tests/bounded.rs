//! The bound that holds against a volume in uninterruptible disk wait.

use doctor::bounded::run;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn sh(script: &str) -> (PathBuf, Vec<String>) {
    (
        PathBuf::from("/bin/sh"),
        vec!["-c".to_owned(), script.to_owned()],
    )
}

#[test]
fn a_prompt_child_is_read_and_reaped() {
    let (program, args) = sh("echo hello; echo trouble >&2; exit 3");
    let answer = run(&program, &args, Duration::from_secs(10), &[]).unwrap();
    assert_eq!(answer.stdout.as_deref(), Some("hello\n"));
    assert_eq!(answer.stderr, "trouble\n");
    assert_eq!(answer.status, Some(3));
    assert!(answer.answered());
}

#[test]
fn a_slow_child_is_abandoned_rather_than_waited_for() {
    // The bound must hold whether or not the child ever finishes. `sleep 30`
    // stands in for uninterruptible disk wait; the point is that `run`
    // returns on schedule and reports the silence as the finding.
    let (program, args) = sh("sleep 30");
    let started = Instant::now();
    let answer = run(&program, &args, Duration::from_millis(300), &[]).unwrap();
    assert!(!answer.answered());
    assert!(answer.status.is_none());
    assert!(answer.pid > 0, "the abandoned pid must be nameable in ps");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the bound did not hold: {:?}",
        started.elapsed()
    );
}

#[test]
fn a_child_that_fills_stderr_does_not_deadlock_the_reader() {
    // Far past a 64 KiB pipe buffer on either stream, interleaved, so a reader
    // draining only one blocks the child on the other. Every chunk here is
    // ALONE bigger than the buffer, so the property does not rest on the total.
    //
    // ⚠ **Four iterations, not four hundred** (#1480). This wrote the same
    // 409,600 bytes in 1024-byte chunks, which spawned 1,600 processes inside a
    // 30 s bound — and on 2026-09-10 that bound blew inside `nix build`, where
    // the whole workspace's tests run at once: `stdout` came back None, meaning
    // the run timed out rather than deadlocked. The flake was the SPAWN COUNT,
    // not the thing under test. Shrink the count, never the timeout: raising the
    // bound would have hidden a real deadlock behind a longer wait.
    let (program, args) = sh("for i in 1 2 3 4; do \
               head -c 102400 /dev/zero | tr '\\0' 'x'; \
               head -c 102400 /dev/zero | tr '\\0' 'y' >&2; \
             done");
    let answer = run(&program, &args, Duration::from_secs(30), &[]).unwrap();
    assert_eq!(answer.stdout.map(|s| s.len()), Some(409_600));
    assert_eq!(answer.stderr.len(), 409_600);
}

#[test]
fn the_child_starts_off_the_callers_working_directory() {
    // The parent's cwd is often the archive volume; a wedged cwd would hang
    // the child before it ran a line.
    let (program, args) = sh("pwd");
    let answer = run(&program, &args, Duration::from_secs(10), &[]).unwrap();
    assert_eq!(answer.stdout.as_deref(), Some("/\n"));
}

/// ⚠ **A child that HANGS never reaches EOF**, so a drain that sends once at
/// the end throws away everything it managed to say — in exactly the run where
/// that is worth having. `doctor`'s archive read prints its volume probe first
/// for this reason: "the disk answered a fixed read instantly while the archive
/// read never returned" is the whole diagnosis, and it used to be discarded.
#[test]
fn what_a_hanging_child_already_said_survives_being_abandoned() {
    let script = "echo 'the disk answered' >&2; sleep 60";
    let answer = doctor::bounded::run(
        std::path::Path::new("/bin/sh"),
        &["-c".to_owned(), script.to_owned()],
        std::time::Duration::from_millis(700),
        &[],
    )
    .expect("spawn");

    assert!(!answer.answered(), "the child must not have finished");
    assert!(
        answer.stderr.contains("the disk answered"),
        "what it said before hanging was lost: {:?}",
        answer.stderr
    );
}

/// …and a child that finishes normally still reports both streams whole.
#[test]
fn a_child_that_finishes_reports_both_streams_whole() {
    let answer = doctor::bounded::run(
        std::path::Path::new("/bin/sh"),
        &["-c".to_owned(), "echo out; echo err >&2".to_owned()],
        std::time::Duration::from_secs(10),
        &[],
    )
    .expect("spawn");

    assert!(answer.answered());
    assert_eq!(answer.stdout.as_deref().map(str::trim), Some("out"));
    assert_eq!(answer.stderr.trim(), "err");
}

use doctor::bounded::State;

#[test]
fn a_ps_that_cannot_be_asked_is_unknown_and_never_gone() {
    // ⚠⚠ The two must not collapse. `ps` does not work inside the nix build
    // sandbox, and for one commit a living child there read as "gone" — the
    // same lie as the assertion this replaced, pointing the other way. A
    // process that certifies a stall must not invent the one fact it exists
    // to report.
    let unknown = doctor::bounded::process_state_via("no-such-ps-binary", std::process::id());
    assert!(
        matches!(unknown, State::Unknown(_)),
        "an unrunnable ps is not evidence of anything: {unknown:?}"
    );
    assert!(
        unknown.explain().contains("unknown"),
        "{}",
        unknown.explain()
    );
    assert_eq!(unknown.label(), "?");
}

#[test]
fn the_state_letters_that_matter_are_told_apart() {
    // `ps` appends flags (`Ss`, `S+`); only the leading letter is the state.
    assert!(
        State::Named("U".to_owned())
            .explain()
            .contains("volume has not answered")
    );
    assert!(
        State::Named("Ss".to_owned())
            .explain()
            .contains("NOT the disk")
    );
    assert!(
        State::Named("R+".to_owned())
            .explain()
            .contains("slow, not blocked")
    );
    assert!(
        State::Gone.explain().contains("exited just after"),
        "gone is a reading, not an absence"
    );
}

#[test]
fn a_live_pid_is_named_and_never_read_as_gone() {
    // ⚠ **This process's own pid, and no child.** An earlier version spawned a
    // `sleep` to look at, and both halves of that were wrong: a freshly spawned
    // process is legitimately `R` before it reaches its nanosleep, so the
    // assertion graded the SCHEDULER — and the spawn itself made the neighbour
    // above flaky, which runs `pwd` on a ten-second clock. Measured under eight
    // burners: 0/40 without these tests, 2/40 with them.
    //
    // What is worth pinning is neither the letter nor the spawn: a pid that is
    // alive must never read as GONE, which is exactly the fault the nix sandbox
    // caught when `ps` could not be asked at all.
    match doctor::bounded::process_state(std::process::id()) {
        State::Named(raw) => assert!(!raw.is_empty(), "a named state is not empty"),
        // `ps` cannot look here; the case that matters then is pinned above.
        State::Unknown(_) => {}
        State::Gone => panic!("this process is running, so it cannot be gone"),
    }
}
