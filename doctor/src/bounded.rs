//! Work you can give up on: a child process the parent abandons instead of
//! waiting for.
//!
//! A starved archive volume puts every reader into uninterruptible disk wait,
//! the doctor included, and the doctor's job is to report exactly that.
//!
//! ⚠ Killing the child does not bound it. A process in uninterruptible wait
//! (`U` in `ps`) does not die on SIGKILL until its I/O completes, so
//! `wait()` after `kill()` blocks as long as the volume does. The only bound
//! that holds is to abandon the child: stop reading, report the timeout, never
//! signal or reap it. It exits on its own once its I/O completes.
//!
//! The child must therefore be disposable: it may still be running after
//! [`run`] returns. Read-only probes qualify; anything that mutates the archive
//! does not.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

/// The child's working directory. The parent's cwd is often the archive
/// volume, and a child that inherits a wedged cwd hangs before running any of
/// our code.
const SAFE_CWD: &str = "/";

/// What a bounded child said, and how long it took to say it.
///
/// `stdout: None` means it never answered inside the bound, which is itself
/// the finding.
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

/// Read a pipe on its own thread, handing each chunk back as it arrives. One per
/// pipe: a child that fills the 64 KiB stderr pipe while the parent reads only
/// stdout blocks forever, indistinguishable from a wedged volume.
///
/// ⚠ Chunk by chunk, not read-to-EOF: a child that hangs never reaches EOF, and
/// what it said before hanging is the useful part. The channel disconnecting
/// reports EOF.
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
/// stays valid after a timeout (the child is left alive), so a log line can
/// name the process to find in `ps`.
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
    let mut child = {
        // ⚠ One spawn at a time in this process. On macOS a pipe is made and
        // marked close-on-exec in two steps, so a child spawned by another
        // thread in between inherits this child's write end, and the read end
        // reaches EOF only when THAT process exits: a prompt child reads as
        // hung for the whole bound (#1480, caught with a `sleep 30` holding
        // another test's pipe). Held for the spawn only, never the wait.
        static SPAWN: Mutex<()> = Mutex::new(());
        let _one = SPAWN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        command.spawn()?
    };
    let pid = child.id();
    let out = drain(child.stdout.take().expect("stdout was piped"));
    let err = drain(child.stderr.take().expect("stderr was piped"));

    let deadline = started + timeout;
    // Everything the pipe has produced, and whether it reached EOF. Queued
    // chunks are taken without waiting first, so output already sent is kept
    // even after the deadline.
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
        // No kill, no wait (see the module docs): both would block as long as
        // the volume does. Forgetting `child` leaves the process to exit on its
        // own.
        std::mem::forget(child);
        return Ok(Answer {
            stdout: None,
            // What it said before it hung.
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
/// Each state points at a different fault: `U` is the volume not answering,
/// `S` is waiting on something other than the disk (a lock), `R` is a read
/// that is merely slow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// `ps` named it. The string is the raw field, flags and all.
    Named(String),
    /// `ps` ran and knows no such process: it finished just after the bound ran
    /// out rather than wedging.
    Gone,
    /// `ps` could not be asked, so nothing is known.
    ///
    /// ⚠ Never collapse this into [`State::Gone`]: a living child would read as
    /// finished. `ps` does not work in the nix build sandbox, so tests reach
    /// this.
    Unknown(String),
}

impl State {
    /// How it reads to somebody who has not memorised `ps`'s letters.
    ///
    /// Only the leading letter is the state; the rest (`Ss`, `S+`) is flags.
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

/// Processes that write to disk in bulk. Named so they are reported at a stall
/// even when not in `U` at that instant: a build between writes is `R` or `S`.
const HEAVY_WRITERS: &[&str] = &[
    "cargo",
    "rustc",
    "ld",
    "ld64",
    "clang",
    "nix",
    "nix-daemon",
    "restic",
    "rsync",
    "git",
    "mds_stores",
    "mdworker_shared",
    "backupd",
];

/// From `ps -A -o pid=,state=,%cpu=,comm=`: every process waiting on a disk
/// (`U`), then every known heavy writer, as `pid state cpu% name`. Names only,
/// never arguments. At most `cap` lines.
///
/// `U` names who was blocked on the disk that second, which includes a busy
/// writer much of the time but is not a byte count.
#[must_use]
pub fn disk_suspects(ps_output: &str, cap: usize) -> Vec<String> {
    let mut waiting = Vec::new();
    let mut writers = Vec::new();
    for line in ps_output.lines() {
        let mut fields = line.split_whitespace();
        let (Some(pid), Some(state), Some(cpu)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let comm = fields.collect::<Vec<_>>().join(" ");
        let name = comm.rsplit('/').next().unwrap_or(&comm);
        let row = format!("{pid} {state} {cpu}% {name}");
        if state.starts_with('U') {
            waiting.push(row);
        } else if HEAVY_WRITERS.contains(&name) {
            writers.push(row);
        }
    }
    waiting.extend(writers);
    waiting.truncate(cap);
    waiting
}

/// [`disk_suspects`] for this machine now; empty when `ps` cannot be asked.
#[must_use]
pub fn disk_suspects_now(cap: usize) -> Vec<String> {
    Command::new("ps")
        .args(["-A", "-o", "pid=,state=,%cpu=,comm="])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| disk_suspects(&String::from_utf8_lossy(&out.stdout), cap))
        .unwrap_or_default()
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
    // An empty stdout means "no such process" only if ps succeeded; where it
    // cannot look (a sandbox) it exits non-zero with nothing on stdout.
    if out.status.success() {
        State::Gone
    } else {
        State::Unknown(format!(
            "{program} said nothing ({})",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}
