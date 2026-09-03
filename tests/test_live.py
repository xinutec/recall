"""Live-transcription helpers (the VAD run loop itself is integration-only)."""

from __future__ import annotations

import io
import os
import queue
import subprocess
import threading
import time
import wave
from datetime import UTC, datetime
from pathlib import Path

from recall.live import (
    _stop_producer,
    drain_to_queue,
    drain_utterances,
    mic_argv,
    write_wav,
)


class _FakeProducer:
    """Stands in for the mic reader Popen. `dies_on_terminate=False` models a reader
    that ignores SIGTERM and must be hard-killed."""

    def __init__(self, *, alive: bool = True, dies_on_terminate: bool = True) -> None:
        self._alive = alive
        self._dies_on_terminate = dies_on_terminate
        self.terminated = False
        self.killed = False

    def poll(self) -> int | None:
        return None if self._alive else 0

    def terminate(self) -> None:
        self.terminated = True
        if self._dies_on_terminate:
            self._alive = False

    def wait(self, timeout: float | None = None) -> int:
        if self._alive and timeout is not None:
            raise subprocess.TimeoutExpired(cmd="ffmpeg", timeout=timeout)
        return 0

    def kill(self) -> None:
        self.killed = True
        self._alive = False


def test_stop_producer_is_a_noop_when_already_exited() -> None:
    producer = _FakeProducer(alive=False)
    _stop_producer(producer)
    assert not producer.terminated
    assert not producer.killed


def test_stop_producer_terminates_a_live_reader() -> None:
    # The mic reader must never survive a live-agent stop — an orphan wedges the shared
    # CoreAudio device and dead-windows capture.
    producer = _FakeProducer(alive=True, dies_on_terminate=True)
    _stop_producer(producer)
    assert producer.terminated
    assert not producer.killed  # a well-behaved reader stops on terminate


def test_stop_producer_hard_kills_a_reader_that_ignores_terminate() -> None:
    producer = _FakeProducer(alive=True, dies_on_terminate=False)
    _stop_producer(producer)
    assert producer.terminated
    assert producer.killed  # it didn't die on terminate, so it gets killed


def test_mic_argv_reads_the_udp_tap_not_the_device() -> None:
    # Live reads the best-effort tap capture publishes, NOT the mic — two CoreAudio
    # clients on one device starve each other, so only capture may hold it.
    argv = mic_argv("")
    assert argv[0] == "ffmpeg"
    assert "avfoundation" not in argv  # NOT a device client
    i = argv.index("-i")
    assert argv[i + 1].startswith("udp://127.0.0.1:")
    assert argv[argv.index("-ar") + 1] == "16000"  # the tap is 16 kHz mono
    assert argv[argv.index("-ac") + 1] == "1"
    assert argv[-1] == "-"  # PCM to stdout, for the existing drain loop


def test_mic_argv_ignores_the_device_arg_now() -> None:
    # The device pin lives on capture (the sole reader); live's --device is vestigial.
    assert mic_argv("USB Condenser Microphone") == mic_argv("")
    assert ":USB Condenser Microphone" not in mic_argv("USB Condenser Microphone")


def test_write_wav_is_valid_mono_16k(tmp_path: Path) -> None:
    pcm = b"\x00\x01" * 16000  # 1 s of 16-bit mono at 16 kHz
    path = tmp_path / "clip.wav"
    write_wav(pcm, path)

    with wave.open(str(path)) as wav:
        assert wav.getnchannels() == 1
        assert wav.getsampwidth() == 2
        assert wav.getframerate() == 16000
        assert wav.getnframes() == 16000


def test_drain_preserves_every_chunk_in_order_then_a_sentinel() -> None:
    chunk = 512
    payload = [bytes([i % 256]) * chunk for i in range(8)]
    frames: queue.Queue[bytes | None] = queue.Queue()

    drain_to_queue(io.BytesIO(b"".join(payload)), frames, chunk)

    got = [frames.get_nowait() for _ in range(len(payload) + 1)]
    assert got[:-1] == payload
    assert got[-1] is None  # end sentinel so the consumer stops too


def test_drain_treats_a_short_final_read_as_end() -> None:
    # A trailing partial chunk (or a clean EOF) means the producer ended — it is not a
    # frame; forward only whole frames, then the sentinel.
    chunk = 512
    whole = bytes(chunk)
    frames: queue.Queue[bytes | None] = queue.Queue()

    drain_to_queue(io.BytesIO(whole + b"\x00\x00\x00"), frames, chunk)

    assert frames.get_nowait() == whole
    assert frames.get_nowait() is None
    assert frames.empty()


def test_drain_never_blocks_the_producer_when_the_consumer_stalls() -> None:
    # The real failure this fixes: sox writes to a fixed-size OS pipe; a stalled VAD
    # consumer leaves the pipe unread, it backs up, and sox's CoreAudio buffer overruns
    # (data discarded / abort). The drain thread must empty the pipe regardless, so the
    # producer streams > a pipe-buffer of data through a deliberately slow consumer with
    # nothing lost and no deadlock.
    chunk = 512
    payload = [bytes([i % 256]) * chunk for i in range(500)]  # 256 KB >> ~64 KB pipe
    read_fd, write_fd = os.pipe()
    frames: queue.Queue[bytes | None] = queue.Queue()

    reader = io.BufferedReader(io.FileIO(read_fd, closefd=True))
    drain = threading.Thread(target=drain_to_queue, args=(reader, frames, chunk))
    drain.start()

    def write_all() -> None:
        with os.fdopen(write_fd, "wb") as sink:
            for c in payload:
                sink.write(c)

    writer = threading.Thread(target=write_all)
    writer.start()

    got: list[bytes] = []
    while True:
        item = frames.get(timeout=5)
        if item is None:
            break
        got.append(item)
        time.sleep(0.001)  # stall on every frame — the drain must not wait on us

    writer.join(timeout=5)
    drain.join(timeout=5)
    assert not writer.is_alive()  # never blocked on a full pipe
    assert got == payload  # every chunk, in order, none discarded


NOW = datetime(2026, 9, 3, 19, 46, tzinfo=UTC)


def test_one_failing_utterance_does_not_end_the_live_stream() -> None:
    # 2026-09-03, twice in one evening: an exception from _emit propagated out of
    # the consumer thread and KILLED it, while the reader carried on reading the
    # mic at ~10% CPU. Live produced nothing for 40 minutes and then 11, with the
    # process up, KeepAlive satisfied and every check green. The stream must
    # survive a bad utterance — the archive re-derives it properly anyway.
    seen: list[bytes] = []

    def emit(pcm: bytes, _start: datetime) -> None:
        if pcm == b"bad":
            msg = "ASR blew up"
            raise RuntimeError(msg)
        seen.append(pcm)

    q: queue.Queue[tuple[bytes, datetime] | None] = queue.Queue()
    q.put((b"bad", NOW))
    q.put((b"good", NOW))
    q.put(None)
    drain_utterances(q, emit)
    assert seen == [b"good"], "the utterance after a failure must still be written"


def test_the_sentinel_still_ends_the_loop() -> None:
    q: queue.Queue[tuple[bytes, datetime] | None] = queue.Queue()
    q.put(None)
    drain_utterances(q, lambda _pcm, _start: None)  # returns rather than hanging


def test_every_utterance_is_transcribed_when_nothing_fails() -> None:
    seen: list[bytes] = []
    q: queue.Queue[tuple[bytes, datetime] | None] = queue.Queue()
    for pcm in (b"one", b"two", b"three"):
        q.put((pcm, NOW))
    q.put(None)
    drain_utterances(q, lambda pcm, _start: seen.append(pcm))
    assert seen == [b"one", b"two", b"three"]
