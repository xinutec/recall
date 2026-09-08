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

/// Read a pipe to EOF on its own thread, handing the bytes back through a
/// channel. Two of these, because a child that fills the 64 KiB stderr pipe
/// while the parent reads only stdout blocks forever — and that deadlock is
/// indistinguishable from the wedge this module exists to survive.
fn drain<R: Read + Send + 'static>(mut pipe: R) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = pipe.read_to_end(&mut buffer);
        let _ = tx.send(buffer);
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
    let collect = |rx: &mpsc::Receiver<Vec<u8>>| -> Option<Vec<u8>> {
        let left = deadline.saturating_duration_since(Instant::now());
        rx.recv_timeout(left).ok()
    };
    let stdout = collect(&out);
    let stderr = collect(&err);

    let seconds = started.elapsed().as_secs_f64();
    let (Some(stdout), Some(stderr)) = (stdout, stderr) else {
        // No kill, no wait — see this module's docstring. Both would block for
        // as long as the volume does. `child` is dropped here, which in Rust
        // does NOT reap: the process is left to finish and exit on its own,
        // which is exactly the intent.
        std::mem::forget(child);
        return Ok(Answer {
            stdout: None,
            stderr: String::new(),
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
