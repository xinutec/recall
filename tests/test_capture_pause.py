"""Self-gating capture: the pipe tears down cleanly on a pause and the entrypoint
re-parks — no cross-process bootout."""

from __future__ import annotations

import subprocess
from collections.abc import Callable
from pathlib import Path

from recall import capture_control, cli
from recall.runner import _run_pipe


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
