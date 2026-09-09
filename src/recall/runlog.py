"""Timestamped logging for the long-running agents.

So the capture lifecycle (listen, pause, teardown, resume) and API actions land in
each agent's `.err.log` on the host's **UTC** clock — the same clock the pause logic
uses (`datetime.now(UTC)`), and the one the phone's logcat clock does *not* share.
That makes the host-side timeline self-consistent and diagnosable without fighting
the phone clock skew.
"""

from __future__ import annotations

import logging
import sys
import threading
import time
from types import TracebackType

log = logging.getLogger(__name__)


def setup() -> None:
    """Configure root logging: INFO, UTC ISO timestamps. Idempotent — basicConfig is
    a no-op once the root logger has a handler, so repeat calls are harmless."""
    logging.Formatter.converter = time.gmtime  # UTC, matching datetime.now(UTC)
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)sZ %(name)s: %(message)s",
        datefmt="%Y-%m-%dT%H:%M:%S",
    )
    catch_uncaught()


def catch_uncaught() -> None:
    """Route uncaught exceptions through `logging`, so they get the same clock.

    ⚠ **Without this, the exceptions that matter most are the ones with no
    timestamp.** `basicConfig` above only reaches messages that go through
    `logging`; an exception nobody caught goes straight to stderr as a bare
    traceback. Measured 2026-09-09, `live.err.log` held 562 `PermissionError:
    /Volumes/Backup`, 562 `FileNotFoundError` and 417 `httpx.ConnectError` in
    exactly that shape — undated, and therefore impossible to line up against
    any of the 45 measured stalls (#1383).

    ⚠ **The THREAD hook is the one this was written for.** live's failure mode is
    a worker thread dying while the reader thread keeps consuming the microphone:
    the process stays up, the transcript stops, and nothing says which thread
    went. `threading.excepthook` names it. That distinction — the thread is dead,
    the process is not — is the whole difference between a hang and a death, and
    it is why the stalls stayed inferable rather than measurable for so long.

    Idempotent in effect: installing twice leaves the same behaviour.
    """

    def on_main(
        exc_type: type[BaseException],
        exc: BaseException,
        tb: TracebackType | None,
    ) -> None:
        # Ctrl-C is how these agents are stopped by hand. Logging it as a fault
        # would put an ERROR in the log every time somebody stopped one, which
        # teaches a reader to skip the level that should mean something.
        if issubclass(exc_type, KeyboardInterrupt):
            sys.__excepthook__(exc_type, exc, tb)
            return
        log.error(
            "uncaught exception — this process is going down",
            exc_info=(exc_type, exc, tb),
        )

    def on_thread(args: threading.ExceptHookArgs) -> None:
        # threading's own default ignores SystemExit; a thread calling exit() is
        # not a fault and never was.
        if args.exc_type is SystemExit:
            return
        name = args.thread.name if args.thread is not None else "unnamed"
        dead = (
            "uncaught exception in thread %s — THAT THREAD IS DEAD, the process is not"
        )
        # `exc_value` is Optional on ExceptHookArgs. It is not None in practice
        # when this fires, but a thread dying is the one event that must still be
        # recorded when the object is missing — the NAME is the diagnostic.
        if args.exc_value is None:
            log.error(dead, name)
            return
        log.error(
            dead, name, exc_info=(args.exc_type, args.exc_value, args.exc_traceback)
        )

    sys.excepthook = on_main
    threading.excepthook = on_thread
