"""The quiet-cleanup HTTP surface: scan, spans, envelope, playback, delete.

The first slice of api.py's decomposition (#1342), registered the same way
sync.py registers its routes: a function taking the app plus what the handlers
need, so this module never imports recall.api back. The scan-job singleton
lives here with the endpoints that own it.
"""

from __future__ import annotations

import logging
from collections.abc import Callable
from datetime import datetime
from pathlib import Path

from fastapi import FastAPI, HTTPException
from fastapi.responses import FileResponse, Response

from recall.api_models import QuietDeleteIn
from recall.ids import AudioSegmentId
from recall.scan_job import ScanJob
from recall.schemas import (
    EnvelopeOut,
    QuietDeletedOut,
    QuietScanOut,
    QuietSpansOut,
)
from recall.store import Store

_log = logging.getLogger("recall.api")

# The one background archive-measuring scan (see recall.scan_job), created on
# first use.
_SCAN_JOB: ScanJob | None = None


def register_quiet_routes(
    app: FastAPI,
    *,
    store_factory: Callable[[], Store],
    require_time: Callable[[str | None], datetime],
) -> None:
    """Mount the /api/quiet/* family. `store_factory` and `require_time` are
    api.py's — passed in rather than imported, so the dependency points one way."""
    _register_scan_routes(app, store_factory)
    _register_span_routes(app, store_factory, require_time)
    _register_delete_route(app, store_factory)


def _register_scan_routes(app: FastAPI, store_factory: Callable[[], Store]) -> None:
    def _scan_job() -> ScanJob:
        """The one scan job, made lazily so it binds to DATA_ROOT as the tests patch
        it."""
        global _SCAN_JOB  # noqa: PLW0603 - one job per process, by design
        if _SCAN_JOB is None:
            _SCAN_JOB = ScanJob(store_factory)
        return _SCAN_JOB

    @app.post("/api/quiet/scan")
    def quiet_scan_start() -> QuietScanOut:
        """Start measuring the archive in the background, and report where it's got to.

        The work outlives the request (~20 minutes of ffmpeg), so this returns at once.
        Calling it while a scan runs joins that one rather than starting a second. Poll
        GET
        for progress; the scan is durable, so closing the page is harmless.
        """
        job = _scan_job()
        job.start()
        return _scan_progress(job)

    @app.get("/api/quiet/scan")
    def quiet_scan_progress() -> QuietScanOut:
        """How far measuring the archive has got, and whether it is still running."""
        return _scan_progress(_scan_job())

    @app.post("/api/quiet/scan/stop")
    def quiet_scan_stop() -> QuietScanOut:
        """Stop the scan after the file it's on. Everything measured stays measured, so
        this
        pauses rather than discards — starting again resumes."""
        job = _scan_job()
        job.stop()
        return _scan_progress(job)

    def _scan_progress(job: ScanJob) -> QuietScanOut:
        progress = job.progress()
        return {
            "running": progress.running,
            "measured": progress.measured,
            "total": progress.total,
            "analysed": progress.analysed,
            "toAnalyse": progress.to_analyse,
        }


def _register_span_routes(
    app: FastAPI,
    store_factory: Callable[[], Store],
    require_time: Callable[[str | None], datetime],
) -> None:
    @app.get("/api/quiet/spans")
    def quiet_spans_list(min_seconds: int = 300) -> QuietSpansOut:
        """The long total-quiet spans, biggest first — see `recall.quiet.rank_spans`.

        Each carries what the microphone heard that was *not* speech (`soundSeconds`,
        its
        loudest moment, and how far that rose above this mic's own floor), so the review
        can
        show a span's bumps and coughs rather than a bare number of minutes.
        """
        from recall.calibrate import event_threshold  # noqa: PLC0415
        from recall.envelope import summarize_sound  # noqa: PLC0415
        from recall.quiet import quiet_spans, rank_spans  # noqa: PLC0415

        store = store_factory()
        try:
            spans = quiet_spans(store, min_duration_s=float(min_seconds))
            measured = []
            for span in spans:
                envelopes = store.audio_envelopes(list(span.audio_ids))
                sound = summarize_sound(
                    [envelopes[a] for a in span.audio_ids if a in envelopes],
                    event_threshold(store, span.source_id),
                    structure=store.span_structure(list(span.audio_ids)),
                )
                measured.append((span, sound))
        finally:
            store.close()

        measured = rank_spans(measured)
        return {
            "items": [
                {
                    "source": span.source_id,
                    "start": span.start.isoformat(),
                    "end": span.end.isoformat(),
                    "durationS": span.duration_s,
                    "audioIds": [int(a) for a in span.audio_ids],
                    "soundSeconds": sound.sound_seconds,
                    "loudestDb": sound.loudest_db,
                    "marginDb": sound.margin_db,
                    "silent": sound.silent,
                    "structure": sound.structure,
                }
                for span, sound in measured
            ]
        }

    @app.get("/api/quiet/envelope")
    def quiet_envelope(
        source: str, start: str, end: str, max_points: int = 1500
    ) -> EnvelopeOut:
        """The waveform of one source over [start, end) — what the review draws to judge
        a
        span: whether it really is dead air throughout, and what broke the quiet at its
        edges. Ask for a window wider than the span to see the sounds that ended it."""
        from recall.envelope import (  # noqa: PLC0415 - keeps ffmpeg/numpy use local
            EnvelopeSegment,
            build_envelope,
        )

        try:
            window_start = require_time(start)
            window_end = require_time(end)
        except ValueError as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc
        if window_end <= window_start:
            raise HTTPException(status_code=400, detail="end must be after start")

        from recall.calibrate import event_threshold  # noqa: PLC0415
        from recall.envelope import (  # noqa: PLC0415
            decode_envelope,
            segment_envelope,
        )

        store = store_factory()
        try:
            rows = store.audio_segments_between(source, window_start, window_end)
            stored = store.audio_envelopes([row[0] for row in rows])
            # What counts as a sound is a property of *this* microphone, measured from
            # it.
            threshold = event_threshold(store, source)
        finally:
            store.close()

        # Read the shape the scan already decoded. Only a segment the scan has never
        # examined falls back to ffmpeg — otherwise opening a 100-minute span would
        # decode
        # its 130 files on the spot, every time. Membership, not truthiness: a segment
        # the
        # scan found undecodable is stored as an *empty* envelope, and that is an answer
        # (draw a gap), not a cache miss to retry against a file that will never decode.
        by_path = {
            row[1]: decode_envelope(stored[row[0]]) for row in rows if row[0] in stored
        }
        envelope = build_envelope(
            [EnvelopeSegment(*row) for row in rows],
            start=window_start,
            end=window_end,
            threshold_db=threshold,
            max_points=max_points,
            envelope_of=lambda path: (
                by_path[path] if path in by_path else segment_envelope(path)
            ),
        )
        return {
            "start": envelope.start.isoformat(),
            "end": envelope.end.isoformat(),
            "bucketS": envelope.bucket_s,
            "thresholdDb": threshold,
            "points": list(envelope.points),
            "segments": [
                {
                    "audioId": int(s.audio_id),
                    "start": s.start.isoformat(),
                    "end": s.end.isoformat(),
                    "meanDb": s.mean_db,
                }
                for s in envelope.segments
            ],
            "events": [
                {
                    "start": e.start.isoformat(),
                    "end": e.end.isoformat(),
                    "peakDb": e.peak_db,
                }
                for e in envelope.events
            ],
        }

    @app.get("/api/quiet/audio/{audio_id}")
    def quiet_audio(audio_id: int) -> Response:
        """Stream one capture segment's raw audio, so a span can be played to confirm
        it's
        quiet before deleting it."""
        store = store_factory()
        try:
            segment = store.audio_segment(AudioSegmentId(audio_id))
        finally:
            store.close()
        if segment is None or not Path(segment.path).exists():
            raise HTTPException(status_code=404, detail="segment not found")
        return FileResponse(segment.path, media_type="audio/ogg")


def _register_delete_route(app: FastAPI, store_factory: Callable[[], Store]) -> None:
    @app.post("/api/quiet/delete")
    def quiet_delete(body: QuietDeleteIn) -> QuietDeletedOut:
        """Hard-delete a confirmed quiet span: its capture segments and everything
        derived,
        plus the Opus files on disk. Reports how many segments went and the bytes freed.

        Logged, before and after. This is the one operation in the app that destroys
        data
        the household cannot get back, and until now it left no record of itself: when
        two
        deletes were fired at once, what they had actually taken could only be
        reconstructed
        by diffing the database against an old snapshot. An irreversible act should say
        what
        it did.

        Deleting the same segments twice is not an error — a duplicate request simply
        finds
        the rows gone and removes nothing. The count says so.
        """
        store = store_factory()
        try:
            span = store.audio_segment_bounds(
                [AudioSegmentId(i) for i in body.audioIds]
            )
            if span is not None:
                source, start, end = span
                _log.info(
                    "DELETE requested: %d segments, %s, %s -> %s",
                    len(body.audioIds),
                    source,
                    start.isoformat(),
                    end.isoformat(),
                )
            paths = store.delete_audio_segments(
                [AudioSegmentId(i) for i in body.audioIds]
            )
        finally:
            store.close()
        freed = 0
        for path in paths:
            file = Path(path)
            try:
                freed += file.stat().st_size
                file.unlink()
            except OSError:
                continue
        _log.info(
            "DELETE done: %d segments removed, %.1f MB freed (%d requested)",
            len(paths),
            freed / 1e6,
            len(body.audioIds),
        )
        return {"deleted": len(paths), "freedBytes": freed}
