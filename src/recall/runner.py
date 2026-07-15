"""Drive capture: a source's PCM producer piped into ffmpeg's segmenter.

The producer (sox/CoreAudio for the mic) streams raw s16le PCM; ffmpeg reads
that stream and writes segment files. This split is what makes capture gap-free:
sox does not drop samples, and ffmpeg only ever sees a clean continuous stream.

This is the part that touches the live device (sox), hence the part that needs
macOS microphone permission.
"""

from __future__ import annotations

import logging
import os
import subprocess
from collections.abc import Callable
from pathlib import Path

from recall.capture import (
    CaptureConfig,
    build_segment_argv,
    container_ext,
    segment_output_pattern,
)
from recall.sources import AudioSource

_log = logging.getLogger("recall.capture")

# How long to let ffmpeg flush + finalise the current segment on SIGTERM before
# force-killing — so a pause never leaves a truncated/corrupt segment file.
_TERM_GRACE_S = 10.0
# How often to check `should_stop` (the pause) while the pipe runs.
_STOP_POLL_S = 1.0


def _run_pipe(
    producer: subprocess.Popen[bytes],
    consumer: subprocess.Popen[bytes],
    should_stop: Callable[[], bool] | None,
    poll_seconds: float,
) -> None:
    """Run until the segmenter exits or `should_stop` (a pause) fires. On a pause,
    close the **producer first** — a TCP listener stops accepting at once, so a phone
    can't connect mid-pause — then let the consumer finalise the current segment on
    the resulting EOF (no audio lost). Force-kill the consumer only if it overruns."""
    if should_stop is None:
        consumer.wait()
        return
    while True:
        try:
            consumer.wait(timeout=poll_seconds)
            return  # exited on its own (producer EOF/error)
        except subprocess.TimeoutExpired:
            if should_stop():
                producer.terminate()  # close the listener now — no accept window
                producer.wait()
                try:
                    consumer.wait(timeout=_TERM_GRACE_S)  # finalises on the EOF
                except subprocess.TimeoutExpired:  # pragma: no cover - belt & braces
                    consumer.kill()
                    consumer.wait()
                return


def record(  # noqa: PLR0913 - capture config + the optional pause hook
    source: AudioSource,
    config: CaptureConfig,
    root: Path,
    *,
    max_seconds: int | None = None,
    should_stop: Callable[[], bool] | None = None,
    poll_seconds: float = _STOP_POLL_S,
    fanout: bool = False,
) -> int:
    """Capture `source` into `root/<source.id>/` as rotating segment files.

    Runs until `max_seconds` elapses (if given), the producer ends, or — when
    `should_stop` is supplied — that predicate returns True (a pause). On a pause
    the pipe is torn down cleanly (current segment finalised) and record() returns,
    so the caller can park on the pause and call record() again to resume. Returns
    the segmenter's exit code. ffmpeg runs with TZ=UTC so filenames carry UTC times.

    `fanout` makes the mic reader also publish a best-effort PCM copy on the live-feed
    UDP tap (recall.sources), so recall-live needn't open the device — only one process
    holds the mic. The tap can't backpressure the segmenter, so the archive is safe.
    """
    out_dir = root / source.id
    out_dir.mkdir(parents=True, exist_ok=True)
    ext = container_ext(config.codec)
    pattern = segment_output_pattern(str(root), source.id, ext=ext)
    producer_argv = source.producer_argv(
        config.sample_rate, config.channels, max_seconds=max_seconds, fanout=fanout
    )
    consumer_argv = build_segment_argv(config, pattern)
    env = {**os.environ, "TZ": "UTC"}

    producer = subprocess.Popen(producer_argv, stdout=subprocess.PIPE, env=env)
    if producer.stdout is None:  # pragma: no cover - Popen with PIPE always sets it
        msg = "producer stdout pipe was not created"
        raise RuntimeError(msg)
    consumer = subprocess.Popen(consumer_argv, stdin=producer.stdout, env=env)
    # Close our copy so the producer sees SIGPIPE if the consumer exits early.
    producer.stdout.close()
    _log.info("listening: %s (%s)", source.id, source.spec or source.kind.value)
    _run_pipe(producer, consumer, should_stop, poll_seconds)
    # Safety net: ensure the producer is gone (a no-op when _run_pipe already closed
    # it on a pause, or when it ended on its own / via SIGPIPE on the unpaused path).
    producer.terminate()
    producer.wait()
    reason = "pause" if should_stop is not None and should_stop() else "producer ended"
    _log.info("stopped: %s (%s)", source.id, reason)
    return consumer.returncode
