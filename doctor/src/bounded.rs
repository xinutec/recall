//! A child process the parent abandons rather than waits for. A starved volume
//! puts every reader into uninterruptible disk wait, and the doctor must still
//! report it.
//!
//! Killing the child does not bound it: in uninterruptible wait (`U` in `ps`)
//! SIGKILL takes effect only when the I/O completes, so `wait()` blocks as long
//! as the volume does. So the parent stops reading and never signals or reaps
//! it; the child exits when its I/O completes. It may outlive [`run`], so it
//! must not write.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

/// The child's working directory: an inherited cwd on a wedged volume would
/// hang it before `main`.
const SAFE_CWD: &str = "/";

/// What a bounded child said, and how long it took. `stdout: None` means it
/// did not answer in time.
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

/// Read a pipe on its own thread, sending each chunk as it arrives; the channel
/// disconnects at EOF. A thread per pipe, since a child blocked on a full
/// stderr pipe looks like a wedged volume. Chunks, not read-to-EOF, so what a
/// hung child said before hanging is kept.
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

/// Run `program`, read what it says, and give up on it after `timeout`. A slow
/// child is an answer, not an error; its `pid` stays valid for `ps`.
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
        // One spawn at a time. macOS makes a pipe and marks it close-on-exec
        // in two steps, so a child another thread spawns in between inherits
        // this one's write end, and a prompt child reads as hung until that
        // process exits (#1480). Held for the spawn only.
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
    // Everything the pipe produced, and whether it reached EOF. Queued chunks
    // are taken first, so output sent before the deadline is kept.
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
        // No kill, no wait (see the module doc).
        std::mem::forget(child);
        return Ok(Answer {
            stdout: None,
            stderr: String::from_utf8_lossy(&err_bytes).into_owned(),
            status: None,
            seconds,
            pid,
        });
    };

    // Both pipes are at EOF, so the child is exiting.
    let status = child.wait()?;
    Ok(Answer {
        stdout: Some(String::from_utf8_lossy(&stdout).into_owned()),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        status: status.code(),
        seconds: started.elapsed().as_secs_f64(),
        pid,
    })
}

/// The kernel's state for an abandoned child: `U` is the volume not answering,
/// `S` waiting on something else (a lock), `R` a read that is merely slow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// `ps`'s raw field, flags included.
    Named(String),
    /// No such process: it finished just after the bound.
    Gone,
    /// `ps` could not be asked, as in the nix build sandbox. Not [`State::Gone`]:
    /// the child may be alive.
    Unknown(String),
}

impl State {
    /// The state in words. Only the first letter is the state; the rest are
    /// flags.
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

    #[must_use]
    pub fn label(&self) -> &str {
        match self {
            State::Named(raw) => raw,
            State::Gone => "gone",
            State::Unknown(_) => "?",
        }
    }
}

/// Bulk disk writers, reported at a stall in any state: between writes a build
/// is `R` or `S`, not `U`.
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

/// From `ps -A -o pid=,state=,%cpu=,comm=`: every process in disk wait (`U`),
/// then every known bulk writer, as `pid state cpu% name`, at most `cap`.
/// Names only, never arguments.
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

/// [`process_state`] with a given `ps`, so a test can name an absent one.
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
    // Empty means no such process only if ps succeeded.
    if out.status.success() {
        State::Gone
    } else {
        State::Unknown(format!(
            "{program} said nothing ({})",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}
