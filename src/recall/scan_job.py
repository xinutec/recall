"""The cleanup scan as a background job on the server, not a loop in a browser tab.

Measuring the archive is ~9k ffmpeg decodes, about twenty minutes. Driven from the
page — one HTTP request per batch, the next fired when the last returns — that work is
hostage to the tab: close it, sleep the laptop, or drop one request and the scan stops.
It stopped silently on a live archive, which is how this module came to exist.

So the page no longer drives anything. It asks for a scan, and watches: the job owns
the work, survives the tab, and reports (measured, total) so the wait is a progress bar
rather than a guess. One job at a time — a second request joins the running scan instead
of starting a competing one, which would decode the same files twice.
"""

from __future__ import annotations

import threading
from collections.abc import Callable
from dataclasses import dataclass

from recall.calibrate import calibrate
from recall.quiet import SWEEPABLE_KINDS, scan_segments
from recall.store import Store


@dataclass(frozen=True)
class ScanProgress:
    """What the page shows: is a scan running, and how far has the archive got."""

    running: bool
    measured: int
    total: int


class ScanJob:
    """A single, restartable background scan. Thread-safe; one run at a time."""

    def __init__(self, open_store: Callable[[], Store]) -> None:
        self._open_store = open_store
        self._lock = threading.Lock()
        self._thread: threading.Thread | None = None
        self._stopping = False

    def start(self) -> None:
        """Begin scanning, unless a scan is already running (then this is a no-op — the
        caller just watches the one already going)."""
        with self._lock:
            if self._thread is not None and self._thread.is_alive():
                return
            self._stopping = False
            self._thread = threading.Thread(
                target=self._run, name="quiet-scan", daemon=True
            )
            self._thread.start()

    def stop(self) -> None:
        """Ask the scan to stop after the file it's on. Measured work is already
        durable, so a stopped scan is paused, not lost: starting again resumes it."""
        self._stopping = True

    def _run(self) -> None:
        store = self._open_store()
        try:
            while (
                scan_segments(store, should_stop=lambda: self._stopping) > 0
                and not self._stopping
            ):
                pass
            # Then measure each mic from what was just read. A threshold is a property
            # of a microphone (recall.calibrate), a new mic starts unknown, and a floor
            # drifts — so it is re-derived every time the archive grows.
            calibrate(store)
        finally:
            store.close()

    def progress(self) -> ScanProgress:
        store = self._open_store()
        try:
            measured, total = store.measured_counts(kinds=SWEEPABLE_KINDS)
        finally:
            store.close()
        running = self._thread is not None and self._thread.is_alive()
        return ScanProgress(running=running, measured=measured, total=total)
