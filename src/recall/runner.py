"""Drive capture: a source's PCM producer piped into ffmpeg's segmenter.

The producer (sox/CoreAudio for the mic) streams raw s16le PCM; ffmpeg reads
that stream and writes segment files. This split is what makes capture gap-free:
sox does not drop samples, and ffmpeg only ever sees a clean continuous stream.

sox's one known failure — its CoreAudio read rarely wedges to digital zeros while
the device stays healthy — is covered here by the dead-segment watchdog: it cycles
the producer when closed segments decode to pure silence (or rotation stalls), so
a wedge costs minutes instead of the rest of the recording.

This is the part that touches the live device (sox), hence the part that needs
macOS microphone permission.
"""

from __future__ import annotations

import logging
import os
import subprocess
import threading
from array import array
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

# Dead-segment watchdog: how often it looks, and how many consecutive digital-silence
# segments mean the producer's device read has wedged. Two segments (not one) because
# a single one could straddle the moment a wedge began.
_WATCH_POLL_S = 30.0
_DEAD_SEGMENTS_TO_CYCLE = 2
# |s16| below this across a whole segment = digital silence. A live room's noise floor
# measures -69..-51 dB here (amplitude 10-90); a wedged CoreAudio read yields exact
# zeros. 2 (~ -84 dB) tolerates dither while never firing on a real, quiet room.
_SILENCE_PEAK = 2


def _segment_is_digital_silence(path: Path) -> bool:
    """True when the segment holds nothing or decodes to pure digital zeros — the
    signature of a wedged device read (a live room's noise floor is never zero).
    Unreadable is NOT a verdict: never cycle on doubt."""
    try:
        if path.stat().st_size == 0:
            return True
        pcm = subprocess.run(
            ["ffmpeg", "-v", "error", "-i", str(path), "-f", "s16le", "-ac", "1", "-"],
            check=True,
            capture_output=True,
        ).stdout
    except (OSError, subprocess.CalledProcessError):
        return False
    pcm = pcm[: len(pcm) - (len(pcm) % 2)]
    if not pcm:
        return True
    samples = array("h", pcm)
    low, high = min(samples), max(samples)
    return max(high, -low) < _SILENCE_PEAK


def _watch_dead_segments(  # noqa: PLR0913 - the watchdog's tuning knobs
    out_dir: Path,
    source_id: str,
    producer: subprocess.Popen[bytes],
    stop: threading.Event,
    *,
    stall_after_s: float,
    on_cycled: Callable[[str], None] | None = None,
    poll_s: float = _WATCH_POLL_S,
) -> None:
    """Cycle the producer when the stream is dead: either the newest CLOSED segments
    decode to digital silence (a wedged sox read keeps delivering zeros, so segments
    keep rotating, tiny and silent), or no new segment file has appeared for far
    longer than the segment length (a producer delivering nothing, so rotation never
    happens). Terminating the producer EOFs the segmenter — flushing whatever audio
    is buffered — record() returns, and the agent's respawn re-opens the device,
    which clears a CoreAudio wedge. Detection costs minutes; the old failure mode
    cost the rest of the recording."""
    dead_streak = 0
    last_checked: str | None = None
    newest_seen: str | None = None
    newest_for_s = 0.0
    while not stop.wait(poll_s):
        names = sorted(p.name for p in out_dir.glob(f"{source_id}-*"))
        if not names:
            continue
        if names[-1] != newest_seen:
            newest_seen = names[-1]
            newest_for_s = 0.0
        else:
            newest_for_s += poll_s
        stalled = newest_for_s >= stall_after_s
        # names[-1] is the open segment; the one before it is the newest CLOSED one.
        closed = names[-2] if len(names) > 1 else None
        if closed is not None and closed != last_checked:
            last_checked = closed
            if _segment_is_digital_silence(out_dir / closed):
                dead_streak += 1
            else:
                dead_streak = 0
        if dead_streak >= _DEAD_SEGMENTS_TO_CYCLE or stalled:
            why = "stalled producer" if stalled else f"{dead_streak} silent segments"
            _log.warning("dead capture stream (%s) — cycling the producer", why)
            if on_cycled is not None:
                try:
                    on_cycled(why)
                except Exception:  # telemetry must never block the cycle
                    _log.exception("on_cycled hook failed (still cycling)")
            producer.terminate()
            return


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


def record(  # noqa: PLR0913 - capture config + the optional pause/telemetry hooks
    source: AudioSource,
    config: CaptureConfig,
    root: Path,
    *,
    max_seconds: int | None = None,
    should_stop: Callable[[], bool] | None = None,
    poll_seconds: float = _STOP_POLL_S,
    fanout: bool = False,
    watch_dead: bool = False,
    watch_poll_s: float = _WATCH_POLL_S,
    on_cycled: Callable[[str], None] | None = None,
) -> int:
    """Capture `source` into `root/<source.id>/` as rotating segment files.

    Runs until `max_seconds` elapses (if given), the producer ends, or — when
    `should_stop` is supplied — that predicate returns True (a pause). On a pause
    the pipe is torn down cleanly (current segment finalised) and record() returns,
    so the caller can park on the pause and call record() again to resume. Returns
    the segmenter's exit code. ffmpeg runs with TZ=UTC so filenames carry UTC times.

    `fanout` makes the segmenter also publish a best-effort PCM copy on the live-feed
    UDP tap (recall.sources), so recall-live needn't open the device — only one process
    holds the mic. The tap can't backpressure the segment output, so the archive is
    safe.

    `watch_dead` arms the dead-segment watchdog (see _watch_dead_segments): a wedged
    or stalled device read cycles the producer, so record() returns and the caller's
    respawn re-opens the device. `on_cycled` (optional) is told why — the caller
    records it durably.
    """
    out_dir = root / source.id
    out_dir.mkdir(parents=True, exist_ok=True)
    ext = container_ext(config.codec)
    pattern = segment_output_pattern(str(root), source.id, ext=ext)
    producer_argv = source.producer_argv(
        config.sample_rate, config.channels, max_seconds=max_seconds
    )
    consumer_argv = build_segment_argv(config, pattern, fanout=fanout)
    env = {**os.environ, "TZ": "UTC"}

    producer = subprocess.Popen(producer_argv, stdout=subprocess.PIPE, env=env)
    if producer.stdout is None:  # pragma: no cover - Popen with PIPE always sets it
        msg = "producer stdout pipe was not created"
        raise RuntimeError(msg)
    consumer = subprocess.Popen(consumer_argv, stdin=producer.stdout, env=env)
    # Close our copy so the producer sees SIGPIPE if the consumer exits early.
    producer.stdout.close()
    _log.info("listening: %s (%s)", source.id, source.spec or source.kind.value)
    watch_stop = threading.Event()
    if watch_dead:
        threading.Thread(
            target=_watch_dead_segments,
            args=(out_dir, source.id, producer, watch_stop),
            kwargs={
                # Rotation normally happens every segment; three lengths of nothing
                # (floored so short test segments don't make it hair-triggered) means
                # the producer is delivering no samples at all.
                "stall_after_s": max(3.0 * config.segment_seconds, 90.0),
                "on_cycled": on_cycled,
                "poll_s": watch_poll_s,
            },
            daemon=True,
        ).start()
    try:
        _run_pipe(producer, consumer, should_stop, poll_seconds)
    finally:
        watch_stop.set()
    # Safety net: ensure the producer is gone (a no-op when _run_pipe already closed
    # it on a pause, or when it ended on its own / via SIGPIPE on the unpaused path).
    producer.terminate()
    producer.wait()
    reason = "pause" if should_stop is not None and should_stop() else "producer ended"
    _log.info("stopped: %s (%s)", source.id, reason)
    return consumer.returncode
