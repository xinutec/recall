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

from recall.analyse import analyse_segments
from recall.calibrate import calibrate
from recall.quiet import scan_segments
from recall.sources import SWEEPABLE_KINDS
from recall.store import Store


@dataclass(frozen=True)
class ScanProgress:
    """What the page shows. Two passes, and the second is the one that matters.

    `measured` is the cheap sweep: every segment's volume and waveform (ffmpeg).
    `analysed` is the speech detector listening to the candidates it turned up — slower,
    and the veto a deletion rests on. A span is offered only once its segments have been
    *heard*, so the list stays empty (rightly) until this has caught up.
    """

    running: bool
    measured: int
    total: int
    analysed: int
    to_analyse: int


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
            # drifts — so it is re-derived every time the archive grows. This also
            # learns
            # each mic's noise fingerprint, which the analysis pass measures against.
            calibrate(store)

            # And now listen. Volume found the candidates; only the speech detector can
            # say whether they are safe to lose (recall.analyse). ~0.6s of model per
            # minute of audio, cached per segment, and never paid twice.
            while (
                analyse_segments(store, should_stop=lambda: self._stopping) > 0
                and not self._stopping
            ):
                pass
        finally:
            store.close()

    def progress(self) -> ScanProgress:
        store = self._open_store()
        try:
            measured, total = store.measured_counts(kinds=SWEEPABLE_KINDS)
            analysed, to_analyse = store.analysed_counts(kinds=SWEEPABLE_KINDS)
        finally:
            store.close()
        running = self._thread is not None and self._thread.is_alive()
        return ScanProgress(
            running=running,
            measured=measured,
            total=total,
            analysed=analysed,
            to_analyse=to_analyse,
        )
