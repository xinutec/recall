"""The excepthooks exist so #1383's stalls are diagnosable.

live's failure mode is a WORKER THREAD dying on an exception while the reader
thread keeps consuming the microphone: the process stays up, the transcript
stops, and the only trace is a bare traceback with no clock and no thread name.
`live.err.log` held 562 `PermissionError`, 562 `FileNotFoundError` and 417
`httpx.ConnectError` that could not be lined up against a single stall for
exactly that reason.
"""

from __future__ import annotations

import logging
import sys
import threading
from collections.abc import Iterator

import pytest

from recall import runlog


@pytest.fixture(autouse=True)
def _restore_hooks() -> Iterator[None]:
    """Never leak a hook into another test — pytest reports through these."""
    sys_hook, thread_hook = sys.excepthook, threading.excepthook
    try:
        yield
    finally:
        sys.excepthook, threading.excepthook = sys_hook, thread_hook


def test_an_uncaught_thread_exception_is_logged_with_the_thread_that_died(
    caplog: pytest.LogCaptureFixture,
) -> None:
    """⚠ THE point of this module for #1383. A bare traceback does not say which
    thread stopped, and 'the process is still up' is what made the stall look
    like a hang rather than a death."""
    runlog.catch_uncaught()

    def explode() -> None:
        msg = "/Volumes/Backup went away"
        raise PermissionError(msg)

    with caplog.at_level(logging.ERROR):
        thread = threading.Thread(target=explode, name="live-sync")
        thread.start()
        thread.join()

    assert "live-sync" in caplog.text, "the dead thread must be named"
    assert "PermissionError" in caplog.text
    assert "/Volumes/Backup went away" in caplog.text


def test_a_thread_death_is_logged_at_error_not_swallowed(
    caplog: pytest.LogCaptureFixture,
) -> None:
    """A hook that logged at INFO would bury the one line that matters, and a
    hook that returned quietly would be worse than the bare traceback it
    replaced."""
    runlog.catch_uncaught()

    with caplog.at_level(logging.DEBUG):
        thread = threading.Thread(target=lambda: 1 // 0, name="vad")
        thread.start()
        thread.join()

    faults = [r for r in caplog.records if r.levelno >= logging.ERROR]
    assert faults, "an uncaught thread exception is a fault, not a note"


def test_a_thread_that_ends_normally_logs_nothing(
    caplog: pytest.LogCaptureFixture,
) -> None:
    """The control. A hook that fired on every thread exit would fill the log
    with noise and make the real death unfindable."""
    runlog.catch_uncaught()

    with caplog.at_level(logging.DEBUG):
        thread = threading.Thread(target=lambda: None, name="quiet")
        thread.start()
        thread.join()

    assert not caplog.records


def test_an_uncaught_main_thread_exception_is_logged(
    caplog: pytest.LogCaptureFixture,
) -> None:
    runlog.catch_uncaught()

    with caplog.at_level(logging.ERROR):
        try:
            msg = "the fleet stopped answering"
            raise ConnectionError(msg)
        except ConnectionError:
            sys.excepthook(*sys.exc_info())

    assert "ConnectionError" in caplog.text
    assert "the fleet stopped answering" in caplog.text


def test_a_keyboard_interrupt_is_not_dressed_up_as_a_fault(
    caplog: pytest.LogCaptureFixture,
) -> None:
    """⚠ Ctrl-C is how these agents are stopped by hand. Logging it at ERROR
    would put a fault in the log every time somebody stopped one, which is the
    fastest way to teach a reader to ignore the level."""
    runlog.catch_uncaught()

    with caplog.at_level(logging.DEBUG):
        try:
            raise KeyboardInterrupt
        except KeyboardInterrupt:
            sys.excepthook(*sys.exc_info())

    faults = [r for r in caplog.records if r.levelno >= logging.ERROR]
    assert not faults, "an interrupt is not a fault"


def test_setup_installs_the_hooks_so_no_agent_has_to_remember(
    caplog: pytest.LogCaptureFixture,
) -> None:
    """Every long-running agent already calls setup(); making the hooks part of
    it is what stops the next agent being the one that did not."""
    sys.excepthook = sys.__excepthook__
    threading.excepthook = threading.__excepthook__

    runlog.setup()

    assert sys.excepthook is not sys.__excepthook__
    assert threading.excepthook is not threading.__excepthook__
