"""Work you can give up on — a child process the parent abandons instead of waiting for.

On 2026-08-10 an unrelated `rm -rf` on the archive volume starved every reader of
it for over an hour. `recall-worker`, `recall-refine`, `recall-sync` and
`recall-doctor` all sat in uninterruptible disk wait; a two-table `COUNT(*)` took
4m19s at 0.03s of user CPU. The doctor is the one that matters here: it is the
process whose whole job is to say the archive is unusable, and it was taken down
by the condition it exists to report (#709).

⚠ **A timeout is not enough, and `subprocess.run(timeout=…)` is not one.** A
process in uninterruptible wait (`U` in `ps`) runs no signal handler and does not
die on SIGKILL — the kernel delivers the signal only once the I/O completes.
CPython answers `TimeoutExpired` with `process.kill()` followed by
`process.wait()`, and `Popen.__exit__` then waits a *second* time (read
`subprocess.run`: both waits are unbounded). So against a wedged volume the
timeout expires on schedule and the caller still never comes back. Wrapping the
read in a thread does not help either: a thread cannot be killed, and
`signal.alarm` never fires.

The only bound that actually holds is to **abandon** the child: stop reading,
report the timeout as the finding, and never signal or reap it. It leaves `U`
when its I/O completes and exits on its own, holding nothing anyone else needs.
That is the whole reason this module exists rather than a call to `subprocess`.

The caller's half of the bargain is that the child must be *disposable* — it may
still be running, and still writing, after `run` returns. Read-only probes
qualify; anything that mutates the archive does not.
"""

from __future__ import annotations

import os
import selectors
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Final

# `python -m` puts the working directory on `sys.path`, and the parent's cwd is
# very often the archive volume itself — a child that scans a wedged directory
# for importable packages hangs before reaching a line of our code, and the bound
# would then cover nothing worth bounding.
SAFE_CWD: Final = Path("/")

_READ_BYTES: Final = 65536

# Abandoned children are kept referenced deliberately. A dropped `Popen` goes on
# CPython's internal `_active` list and is reaped by the next `Popen()` call,
# which is harmless but destroys the only handle anyone has on a process stuck in
# `U` state — and makes "did we abandon one?" unanswerable from inside the run.
_ABANDONED: list[subprocess.Popen[bytes]] = []


@dataclass(frozen=True)
class Answer:
    """What a bounded child said, and how long it took to say it.

    `stdout is None` means it never answered inside the bound. That is a reading
    in its own right, not a missing one: it is the finding.
    """

    stdout: str | None
    stderr: str
    returncode: int | None
    seconds: float
    pid: int

    @property
    def answered(self) -> bool:
        return self.stdout is not None


def abandoned() -> list[subprocess.Popen[bytes]]:
    """The children this process gave up on and is deliberately still holding."""
    return list(_ABANDONED)


def forget_abandoned() -> None:
    """Drop the references — for tests that clean up after themselves."""
    _ABANDONED.clear()


def _drain(
    proc: subprocess.Popen[bytes], deadline: float
) -> tuple[bytes, bytes] | None:
    """Read both pipes to EOF, or give up at `deadline`.

    Both, and concurrently: a child that fills the 64 KiB stderr pipe while the
    parent reads only stdout blocks forever, and that deadlock is indistinguish-
    able from the wedge this module exists to survive.
    """
    assert proc.stdout is not None and proc.stderr is not None
    buffers: dict[int, bytearray] = {}
    selector = selectors.DefaultSelector()
    for pipe in (proc.stdout, proc.stderr):
        selector.register(pipe, selectors.EVENT_READ)
        buffers[pipe.fileno()] = bytearray()
    out_fd, err_fd = proc.stdout.fileno(), proc.stderr.fileno()

    try:
        open_pipes = 2
        while open_pipes:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return None
            ready = selector.select(remaining)
            if not ready:
                return None
            for key, _ in ready:
                chunk = os.read(key.fd, _READ_BYTES)
                if chunk:
                    buffers[key.fd] += chunk
                else:
                    selector.unregister(key.fileobj)
                    open_pipes -= 1
    finally:
        selector.close()
    return bytes(buffers[out_fd]), bytes(buffers[err_fd])


def run(
    argv: list[str],
    *,
    timeout_s: float,
    cwd: Path = SAFE_CWD,
    env: dict[str, str] | None = None,
) -> Answer:
    """Run `argv`, read what it says, and give up on it after `timeout_s`.

    Never raises on a slow child: not answering is the answer. The returned
    `pid` stays valid after a timeout — the child is still alive, on purpose —
    so a log line can name the process an operator will find in `ps` in `U`
    state.
    """
    started = time.monotonic()
    proc = subprocess.Popen(
        argv,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        cwd=cwd,
        env=env,
    )
    drained = _drain(proc, started + timeout_s)
    seconds = time.monotonic() - started

    if drained is None:
        # No kill, no wait — see this module's docstring. Both would block for
        # as long as the volume does, which is the failure being avoided.
        _ABANDONED.append(proc)
        return Answer(None, "", None, seconds, proc.pid)

    out, err = drained
    # Both pipes are at EOF, so the child has closed them and is exiting; this
    # wait is the reaping, not a bet on the volume.
    returncode = proc.wait()
    for pipe in (proc.stdout, proc.stderr):
        if pipe is not None:
            pipe.close()
    return Answer(
        out.decode(errors="replace"),
        err.decode(errors="replace"),
        returncode,
        seconds,
        proc.pid,
    )
