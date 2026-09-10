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
