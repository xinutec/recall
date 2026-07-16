"""Self-gating capture: the pipe tears down cleanly on a pause and the entrypoint
re-parks — no cross-process bootout. Plus the dead-segment watchdog: a wedged or
stalled device read cycles the producer instead of silently recording nothing."""

from __future__ import annotations

import subprocess
import threading
import time
from collections.abc import Callable
from pathlib import Path

from recall import capture_control, cli
from recall.runner import _run_pipe, _segment_is_digital_silence, _watch_dead_segments


class _FakeProducer:
    """Stands in for the ffmpeg producer (e.g. a TCP listener) Popen."""

    def __init__(self) -> None:
        self.terminated = False

    def terminate(self) -> None:
        self.terminated = True

    def wait(self, timeout: float | None = None) -> int:
        return 0


class _FakeConsumer:
    """Stands in for the ffmpeg segmenter Popen. Exits (EOF) once the producer it
    reads from is terminated — mirroring how closing the listener finalises it."""

    def __init__(self, producer: _FakeProducer) -> None:
        self.producer = producer
        self.killed = False
        self.returncode = 0

    def wait(self, timeout: float | None = None) -> int:
        if timeout is None or self.producer.terminated:
            return 0  # blocking wait, or EOF after the producer closed
        raise subprocess.TimeoutExpired(cmd="ffmpeg", timeout=timeout)

    def terminate(self) -> None:  # pragma: no cover - not used on the EOF path
        pass

    def kill(self) -> None:  # pragma: no cover - only if the grace window expires
        self.killed = True


def test_run_pipe_blocks_when_not_gated() -> None:
    producer = _FakeProducer()
    consumer = _FakeConsumer(producer)
    _run_pipe(producer, consumer, None, 0.01)  # type: ignore[arg-type]
    assert not producer.terminated  # ran to completion, nothing torn down


def test_run_pipe_closes_listener_first_on_pause() -> None:
    producer = _FakeProducer()
    consumer = _FakeConsumer(producer)
    polls = [0]

    def should_stop() -> bool:
        polls[0] += 1
        return polls[0] >= 2  # paused on the 2nd poll

    _run_pipe(producer, consumer, should_stop, 0.001)  # type: ignore[arg-type]
    assert producer.terminated  # listener closed first — no accept window mid-pause
    assert not consumer.killed  # consumer finalised on the EOF, within the grace


def _wav(path: Path, filt: str) -> None:
    """A 0.2s wav from an ffmpeg synthetic source (sine=... or anullsrc=...)."""
    subprocess.run(
        ["ffmpeg", "-v", "error", "-f", "lavfi", "-i", filt, "-t", "0.2", str(path)],
        check=True,
    )


def test_digital_silence_verdicts(tmp_path: Path) -> None:
    empty = tmp_path / "usb-20260716T120000.wav"
    empty.write_bytes(b"")
    assert _segment_is_digital_silence(empty)  # nothing at all
    silent = tmp_path / "usb-20260716T120100.wav"
    _wav(silent, "anullsrc=r=16000:cl=mono")
    assert _segment_is_digital_silence(silent)  # decodes to pure zeros
    live = tmp_path / "usb-20260716T120200.wav"
    _wav(live, "sine=frequency=440:sample_rate=16000")
    assert not _segment_is_digital_silence(live)
    # unreadable/vanished is NOT a verdict — never cycle on doubt
    assert not _segment_is_digital_silence(tmp_path / "usb-nope.wav")


def test_watchdog_cycles_after_two_dead_closed_segments(tmp_path: Path) -> None:
    # The wedge shape: sox keeps delivering digital zeros, segments keep rotating,
    # each closed one is dead. Two consecutive dead CLOSED segments (the open newest
    # is never judged) → the producer is cycled and the reason reported.
    usb = tmp_path / "usb"
    usb.mkdir()
    (usb / "usb-20260716T120000.opus").write_bytes(b"")  # closed, dead
    (usb / "usb-20260716T120100.opus").write_bytes(b"")  # newest for now
    producer = _FakeProducer()
    stop = threading.Event()
    cycled: list[str] = []
    watcher = threading.Thread(
        target=_watch_dead_segments,
        args=(usb, "usb", producer, stop),
        kwargs={"stall_after_s": 999.0, "on_cycled": cycled.append, "poll_s": 0.02},
        daemon=True,
    )
    watcher.start()
    time.sleep(0.15)  # the first dead closed segment gets counted
    # rotation: 120100 closes (still dead), a new open segment appears
    (usb / "usb-20260716T120200.opus").write_bytes(b"")
    watcher.join(timeout=3.0)
    assert not watcher.is_alive()
    assert producer.terminated
    assert cycled == ["2 silent segments"]
    stop.set()


def test_watchdog_live_audio_resets_the_streak(tmp_path: Path) -> None:
    # dead, then live, then dead: never two CONSECUTIVE dead segments — no cycle.
    usb = tmp_path / "usb"
    usb.mkdir()
    (usb / "usb-20260716T120000.opus").write_bytes(b"")  # closed, dead
    _wav(usb / "usb-20260716T120100.wav", "sine=frequency=440:sample_rate=16000")
    producer = _FakeProducer()
    stop = threading.Event()
    watcher = threading.Thread(
        target=_watch_dead_segments,
        args=(usb, "usb", producer, stop),
        kwargs={"stall_after_s": 999.0, "on_cycled": None, "poll_s": 0.02},
        daemon=True,
    )
    watcher.start()
    time.sleep(0.15)  # counts 120000: streak 1
    (usb / "usb-20260716T120200.opus").write_bytes(b"")  # closes the LIVE 120100
    time.sleep(0.15)  # live segment resets the streak
    (usb / "usb-20260716T120300.opus").write_bytes(b"")  # closes dead 120200: streak 1
    time.sleep(0.15)
    stop.set()
    watcher.join(timeout=2.0)
    assert not producer.terminated  # never reached two consecutive


def test_watchdog_cycles_a_stalled_producer(tmp_path: Path) -> None:
    # The stall shape: no samples at all → rotation never happens → the newest file
    # never changes. The dead-streak path can't see this (nothing closes), so the
    # stall clock catches it. Runs synchronously: the cycle returns.
    usb = tmp_path / "usb"
    usb.mkdir()
    (usb / "usb-20260716T120000.opus").write_bytes(b"")
    producer = _FakeProducer()
    cycled: list[str] = []
    _watch_dead_segments(
        usb,
        "usb",
        producer,  # type: ignore[arg-type]
        threading.Event(),
        stall_after_s=0.05,
        on_cycled=cycled.append,
        poll_s=0.02,
    )
    assert producer.terminated
    assert cycled == ["stalled producer"]


def test_serve_paused_aware_exits_when_a_run_ends_unpaused(
    tmp_path: Path, monkeypatch: object
) -> None:
    monkeypatch.setattr(  # type: ignore[attr-defined]
        capture_control, "wait_until_unpaused", lambda *a, **k: None
    )
    monkeypatch.setattr(capture_control, "is_paused", lambda *a: False)  # type: ignore[attr-defined]
    runs: list[int] = []

    def run_once(_stop: Callable[[], bool]) -> int:
        runs.append(1)
        return 7

    assert cli._serve_paused_aware(tmp_path, run_once) == 7
    assert len(runs) == 1  # ran once, not paused → exit (KeepAlive respawns)


def test_serve_paused_aware_reparks_after_a_pause(
    tmp_path: Path, monkeypatch: object
) -> None:
    monkeypatch.setattr(  # type: ignore[attr-defined]
        capture_control, "wait_until_unpaused", lambda *a, **k: None
    )
    # After the 1st run a pause is active (re-park + run again); after the 2nd it's
    # clear (exit).
    states = iter([True, False])
    monkeypatch.setattr(  # type: ignore[attr-defined]
        capture_control, "is_paused", lambda *a: next(states)
    )
    runs: list[int] = []

    def run_once(_stop: Callable[[], bool]) -> int:
        runs.append(1)
        return 0

    cli._serve_paused_aware(tmp_path, run_once)
    assert len(runs) == 2  # stopped by a pause, re-parked, ran again, then exited
