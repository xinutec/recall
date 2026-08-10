"""A child process the parent can give up on.

Every assertion here is about the *giving up*, not the running — `subprocess`
already runs things. The property under test is that a parent whose child never
answers comes back anyway, and comes back **without killing or reaping it**,
because a process in uninterruptible disk wait cannot be killed and waiting for
one is how the health check joined the outage it exists to report (#709).
"""

from __future__ import annotations

import os
import subprocess
import sys
import time
from collections.abc import Iterator

import pytest

from recall import bounded


@pytest.fixture(autouse=True)
def _no_strays() -> Iterator[None]:
    """Every child this file abandons is cleaned up, so pytest leaves none behind."""
    yield
    for child in bounded.abandoned():
        child.kill()
        child.wait()
    bounded.forget_abandoned()


def _py(code: str) -> list[str]:
    return [sys.executable, "-c", code]


def test_a_child_that_answers_is_read_in_full() -> None:
    # Far past a pipe buffer (64 KiB on macOS): a reader that waits for exit
    # before draining deadlocks here, and a child that fills the pipe while the
    # parent is not reading is indistinguishable from a wedged one.
    answer = bounded.run(_py("print('x' * 300000)"), timeout_s=60.0)
    assert answer.stdout is not None
    assert answer.stdout.strip() == "x" * 300000
    assert answer.returncode == 0


def test_stderr_is_kept_and_does_not_block_stdout() -> None:
    answer = bounded.run(
        _py("import sys; sys.stderr.write('y' * 300000); print('done')"),
        timeout_s=60.0,
    )
    assert answer.stdout is not None
    assert answer.stdout.strip() == "done"
    assert len(answer.stderr) == 300000


def test_a_failing_child_is_an_answer_with_its_reason() -> None:
    answer = bounded.run(_py("raise SystemExit('nope')"), timeout_s=60.0)
    assert answer.returncode == 1
    assert "nope" in answer.stderr


def test_a_silent_child_does_not_hold_the_parent_for_its_lifetime() -> None:
    started = time.monotonic()
    answer = bounded.run(_py("import time; time.sleep(600)"), timeout_s=0.5)
    waited = time.monotonic() - started
    assert answer.stdout is None, "a sleeping child was read as an answer"
    assert waited < 30.0, f"the parent waited {waited:.1f}s on a 0.5s bound"


def test_an_unanswering_child_is_abandoned_rather_than_killed() -> None:
    """The one property that makes this module worth having.

    ⚠ A process in uninterruptible disk wait does not die on SIGKILL — the
    kernel delivers the signal only once the I/O completes — and CPython's
    `subprocess.run` answers `TimeoutExpired` with `kill()` then `wait()`
    (`Popen.__exit__` then waits a second time). Against a wedged volume both
    waits block for as long as the wedge lasts, so the timeout expires and the
    caller still never returns. Leaving the child running is what proves this
    one neither signals nor reaps.
    """
    answer = bounded.run(_py("import time; time.sleep(600)"), timeout_s=0.5)
    assert answer.stdout is None

    os.kill(answer.pid, 0)  # raises if it was reaped: the pid would be gone
    state = subprocess.run(
        ["ps", "-o", "state=", "-p", str(answer.pid)],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    assert state, f"pid {answer.pid} is gone — it was killed and reaped"
    assert not state.startswith("Z"), f"pid {answer.pid} is a zombie: it was killed"


def test_an_abandoned_child_is_held_rather_than_leaked() -> None:
    """Kept referenced on purpose, so nothing reaps it behind our back.

    A dropped `Popen` is added to CPython's `subprocess._active` and reaped by
    the next `Popen()` call — harmless, but it makes "was it abandoned?"
    unanswerable, and the pid is the only handle an operator has on a process
    stuck in `U` state.
    """
    answer = bounded.run(_py("import time; time.sleep(600)"), timeout_s=0.5)
    assert [child.pid for child in bounded.abandoned()] == [answer.pid]


def test_a_child_that_answers_is_not_left_behind() -> None:
    bounded.run(_py("print('hi')"), timeout_s=60.0)
    assert bounded.abandoned() == []


def test_the_child_does_not_start_in_a_directory_that_may_be_wedged() -> None:
    """`python -m` puts the working directory on `sys.path`.

    The parent's cwd is very often the archive volume, and a child that scans it
    for importable packages hangs before reaching a line of recall's own code —
    the bound would then cover nothing that matters.
    """
    answer = bounded.run(_py("import os; print(os.getcwd())"), timeout_s=60.0)
    assert answer.stdout is not None
    assert answer.stdout.strip() == "/"
