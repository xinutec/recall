//! Work you can give up on — a child process the parent abandons instead of
//! waiting for.
//!
//! On 2026-08-10 an unrelated `rm -rf` on the archive volume starved every
//! reader of it for over an hour. The worker, refine, sync and the doctor all
//! sat in uninterruptible disk wait; a two-table `COUNT(*)` took 4m19s at 0.03s
//! of user CPU. The doctor is the one that matters here: it is the process
//! whose whole job is to say the archive is unusable, and it was taken down by
//! the condition it exists to report (#709).
//!
//! ⚠ **A timeout is not enough, and killing the child is not one either.** A
//! process in uninterruptible wait (`U` in `ps`) runs no signal handler and
//! does not die on SIGKILL — the kernel delivers the signal only once the I/O
//! completes. So `wait()`-after-`kill()` blocks for exactly as long as the
//! volume does, which is the failure being avoided.
//!
//! The only bound that actually holds is to **abandon** the child: stop
//! reading, report the timeout as the finding, and never signal or reap it. It
//! leaves `U` when its I/O completes and exits on its own, holding nothing
//! anyone else needs.
//!
//! The caller's half of the bargain is that the child must be *disposable* — it
//! may still be running, and still writing, after [`run`] returns. Read-only
//! probes qualify; anything that mutates the archive does not.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// `std::process` puts nothing on the child's search path from the parent's
/// working directory, but the parent's cwd is very often the archive volume
/// itself — and a child that inherits a wedged cwd hangs before reaching a line
/// of our code, so the bound would cover nothing worth bounding.
const SAFE_CWD: &str = "/";

/// What a bounded child said, and how long it took to say it.
///
/// `stdout: None` means it never answered inside the bound. That is a reading
/// in its own right, not a missing one: it is the finding.
#[derive(Debug)]
pub struct Answer {
    pub stdout: Option<String>,
    pub stderr: String,
    pub status: Option<i32>,
    pub seconds: f64,
    pub pid: u32,
}

impl Answer {
    pub fn answered(&self) -> bool {
        self.stdout.is_some()
    }
}

/// Read a pipe on its own thread, handing each chunk back as it arrives. Two of
/// these, because a child that fills the 64 KiB stderr pipe while the parent
/// reads only stdout blocks forever — and that deadlock is indistinguishable
/// from the wedge this module exists to survive.
///
/// ⚠ **Chunk by chunk, NOT read-to-EOF.** A child that hangs never reaches EOF,
/// so a single send at the end means everything it managed to say before
/// hanging is thrown away — in exactly the run where it is worth having. The
/// channel disconnecting is how EOF is reported instead.
fn drain<R: Read + Send + 'static>(mut pipe: R) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut chunk = [0_u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    if tx.send(chunk[..read].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    rx
}

/// Run `argv`, read what it says, and give up on it after `timeout`.
///
/// Never fails on a slow child: not answering is the answer. The returned `pid`
/// stays valid after a timeout — the child is still alive, on purpose — so a
/// log line can name the process an operator will find in `ps` in `U` state.
pub fn run(
    program: &Path,
    args: &[String],
    timeout: Duration,
    extra_env: &[(String, String)],
) -> std::io::Result<Answer> {
    let started = Instant::now();
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(PathBuf::from(SAFE_CWD))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in extra_env {
        command.env(key, value);
    }
    let mut child = command.spawn()?;
    let pid = child.id();
    let out = drain(child.stdout.take().expect("stdout was piped"));
    let err = drain(child.stderr.take().expect("stderr was piped"));

    let deadline = started + timeout;
    // Everything the pipe has produced, and whether it reached EOF. ⚠ Queued
    // chunks are taken WITHOUT waiting first, so a pipe that already said
    // something still reports it after the deadline has passed — which is the
    // whole point of streaming them.
    let collect = |rx: &mpsc::Receiver<Vec<u8>>| -> (Vec<u8>, bool) {
        let mut all = Vec::new();
        loop {
            match rx.try_recv() {
                Ok(chunk) => {
                    all.extend_from_slice(&chunk);
                    continue;
                }
                Err(mpsc::TryRecvError::Disconnected) => return (all, true),
                Err(mpsc::TryRecvError::Empty) => {}
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return (all, false);
            }
            match rx.recv_timeout(left) {
                Ok(chunk) => all.extend_from_slice(&chunk),
                Err(mpsc::RecvTimeoutError::Disconnected) => return (all, true),
                Err(mpsc::RecvTimeoutError::Timeout) => return (all, false),
            }
        }
    };
    let (out_bytes, out_done) = collect(&out);
    let (err_bytes, err_done) = collect(&err);
    let stdout = out_done.then_some(out_bytes);
    let stderr = err_done.then(|| err_bytes.clone());

    let seconds = started.elapsed().as_secs_f64();
    let (Some(stdout), Some(stderr)) = (stdout, stderr) else {
        // No kill, no wait — see this module's docstring. Both would block for
        // as long as the volume does. `child` is dropped here, which in Rust
        // does NOT reap: the process is left to finish and exit on its own,
        // which is exactly the intent.
        std::mem::forget(child);
        return Ok(Answer {
            stdout: None,
            // ⚠ What it managed to SAY before it hung. This used to be empty,
            // which threw away the only report from the one run that matters.
            stderr: String::from_utf8_lossy(&err_bytes).into_owned(),
            status: None,
            seconds,
            pid,
        });
    };

    // Both pipes are at EOF, so the child has closed them and is exiting; this
    // wait is the reaping, not a bet on the volume.
    let status = child.wait()?;
    Ok(Answer {
        stdout: Some(String::from_utf8_lossy(&stdout).into_owned()),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        status: status.code(),
        seconds: started.elapsed().as_secs_f64(),
        pid,
    })
}

/// What state the kernel has an abandoned child in.
///
/// ⚠⚠ **Because the log line ASSERTED it.** "it is in uninterruptible disk
/// wait" was printed unconditionally on every abandonment, so hundreds of them
/// carried a claim nobody had checked — and the whole diagnosis turns on it:
/// `U` is the volume not answering, `S` is the child waiting on something that
/// is not the disk at all (a database lock has that shape), and `R` is a read
/// that is merely slow. Three different faults that the same sentence described.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// `ps` named it. The string is the raw field, flags and all.
    Named(String),
    /// `ps` ran and knows no such process: it finished just after the bound ran
    /// out rather than wedging, which is its own reading.
    Gone,
    /// ⚠ **`ps` could not be asked**, so nothing is known — and this must never
    /// collapse into [`State::Gone`]. It did, for one commit: `ps` does not work
    /// inside the nix build sandbox, and a living child there read as "gone".
    /// That is the same lie as the assertion this type replaced, in the other
    /// direction.
    Unknown(String),
}

impl State {
    /// How it reads to somebody who has not memorised `ps`'s letters.
    ///
    /// The rest of the field is flags — `Ss`, `S+` — and only the leading state
    /// letter says what the process is doing.
    #[must_use]
    pub fn explain(&self) -> String {
        match self {
            State::Gone => "it exited just after the bound ran out".to_owned(),
            State::Unknown(why) => format!("state unknown: {why}"),
            State::Named(raw) => match raw.chars().next() {
                Some('U') => "uninterruptible disk wait — the volume has not answered",
                Some('S' | 'I') => {
                    "interruptible sleep — waiting on something that is NOT the disk"
                }
                Some('R') => "runnable — the read is slow, not blocked",
                Some('T') => "stopped",
                Some('Z') => "a zombie, so it has already exited",
                _ => "an unrecognised state",
            }
            .to_owned(),
        }
    }

    /// What to print for the state itself.
    #[must_use]
    pub fn label(&self) -> &str {
        match self {
            State::Named(raw) => raw,
            State::Gone => "gone",
            State::Unknown(_) => "?",
        }
    }
}

/// Ask `ps` what state `pid` is in.
#[must_use]
pub fn process_state(pid: u32) -> State {
    process_state_via("ps", pid)
}

/// The same, against a named `ps`, so a test can ask an absent one.
#[must_use]
pub fn process_state_via(program: &str, pid: u32) -> State {
    let out = match Command::new(program)
        .args(["-o", "state=", "-p", &pid.to_string()])
        .output()
    {
        Ok(out) => out,
        Err(e) => return State::Unknown(format!("cannot run {program}: {e}")),
    };
    let state = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if !state.is_empty() {
        return State::Named(state);
    }
    // ⚠ An empty stdout means "no such process" ONLY if ps itself succeeded.
    // Where it cannot look — a sandbox, a stripped image — it exits non-zero
    // with nothing on stdout, which is indistinguishable by stdout alone.
    if out.status.success() {
        State::Gone
    } else {
        State::Unknown(format!(
            "{program} said nothing ({})",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}
