"""Single-port audio ingest: the opening handshake by which a device announces
itself, so many devices share one port instead of one ffmpeg listener each."""

from __future__ import annotations

import errno
import json
import socket
import threading
import time
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import cast, override

import pytest

from recall import capture_control
from recall.capture import CaptureConfig, StreamMeter
from recall.store import Store
from recall.stream_server import (
    Handshake,
    handle_connection,
    parse_handshake,
    read_handshake,
    serve,
)


def _free_port() -> int:
    probe = socket.socket()
    probe.bind(("127.0.0.1", 0))
    port = int(probe.getsockname()[1])
    probe.close()
    return port


def test_parse_handshake_reads_id_and_audio_params() -> None:
    h = parse_handshake('{"id":"kitchen","rate":48000,"channels":1}')
    assert h == Handshake(source_id="kitchen", sample_rate=48000, channels=1)


def test_parse_handshake_defaults_audio_params() -> None:
    # rate/channels optional — the household default is 48k mono.
    h = parse_handshake('{"id":"pixel9-a1b2"}')
    assert h == Handshake(source_id="pixel9-a1b2", sample_rate=48000, channels=1)


def test_parse_handshake_rejects_unsafe_or_missing_id() -> None:
    assert parse_handshake('{"id":"../etc/passwd","rate":48000}') is None
    assert parse_handshake('{"id":""}') is None
    assert parse_handshake('{"rate":48000}') is None  # no id
    assert parse_handshake("not json at all") is None


def test_read_handshake_stops_at_newline_without_consuming_pcm() -> None:
    # The server must read EXACTLY the handshake line — the PCM after the newline
    # belongs to ffmpeg, so over-reading even one byte of it would drop audio.
    payload = b'{"id":"kitchen"}\n\x01\x02\x03\x04PCMDATA'
    pos = 0

    def recv(n: int) -> bytes:
        nonlocal pos
        chunk = payload[pos : pos + n]
        pos += len(chunk)
        return chunk

    handshake = read_handshake(recv)
    assert handshake == Handshake(source_id="kitchen", sample_rate=48000, channels=1)
    # everything from the read cursor on is untouched PCM, left for ffmpeg
    assert payload[pos:] == b"\x01\x02\x03\x04PCMDATA"


def test_read_handshake_rejects_overlong_or_eof() -> None:
    # A client that never sends a newline must not let us read forever.
    assert read_handshake(lambda _n: b"x", max_bytes=64) is None
    # EOF before the newline is a failed handshake, not a hang.
    assert read_handshake(lambda _n: b"") is None


def test_meter_counts_pure_zeros_without_calling_them_audible() -> None:
    # Digital zeros are what a dead capture path produces: bytes flow, no signal.
    m = StreamMeter(sample_rate=48000, channels=1)
    assert m.feed(b"\x00\x00" * 48000) == 0  # 1s of digital silence
    assert m.bytes_total == 96000
    assert m.peak_db is None  # not one non-zero sample
    assert m.first_audible_s is None


def test_meter_feed_returns_each_chunks_own_peak() -> None:
    # The chunk peak (not the running max) is what gates the liveness marker: a loud
    # stream that goes digitally silent must stop refreshing it.
    m = StreamMeter(sample_rate=48000, channels=1)
    assert m.feed((1000).to_bytes(2, "little", signed=True) * 480) == 1000
    assert m.feed(b"\x00\x00" * 480) == 0
    assert m.peak == 1000  # the running max still remembers the loud chunk


def test_meter_reports_near_silence_as_a_level_but_not_audible() -> None:
    # The pixel9 failure signature: amplitude-1 samples ≈ -90 dB. The meter must
    # REPORT that level (it's the diagnostic) while still not calling it audible.
    m = StreamMeter(sample_rate=48000, channels=1)
    m.feed(b"\x01\x00" * 4800)  # 0.1s at amplitude 1
    assert m.peak_db == -90.3
    assert m.first_audible_s is None


def test_meter_finds_the_first_audible_sample_and_the_peak() -> None:
    m = StreamMeter(sample_rate=48000, channels=1)
    m.feed(b"\x00\x00" * 48000)  # 1s of silence first
    m.feed((1000).to_bytes(2, "little", signed=True) * 4800)  # then real signal
    assert m.first_audible_s == pytest.approx(1.0, abs=0.01)
    assert m.peak_db == -30.3  # 20*log10(1000/32768)


def test_meter_hears_a_negative_swing() -> None:
    m = StreamMeter(sample_rate=48000, channels=1)
    m.feed((-1000).to_bytes(2, "little", signed=True) * 480)
    assert m.first_audible_s == 0.0
    assert m.peak_db == -30.3


def test_meter_handles_a_sample_split_across_chunks() -> None:
    # recv() chunks don't respect sample boundaries; the half-sample must carry over.
    m = StreamMeter(sample_rate=48000, channels=1)
    m.feed(b"\x00")  # first half of a sample
    m.feed(b"\x10")  # completes 0x1000 = 4096 — audible
    assert m.bytes_total == 2
    assert m.first_audible_s == 0.0
    assert m.peak_db == pytest.approx(-18.1, abs=0.1)


def test_handle_connection_records_ingest_telemetry(tmp_path: Path) -> None:
    # A connection leaves a durable
    # record of what the device actually SENT — bytes and level — plus which segment
    # file the close flushed. This is what settles "silent stream" vs "no stream".
    server_sock, client_sock = socket.socketpair()

    def device() -> None:
        client_sock.sendall(b'{"id":"kitchen"}\n')
        client_sock.sendall((2000).to_bytes(2, "little", signed=True) * 48000)
        client_sock.close()

    sender = threading.Thread(target=device)
    sender.start()
    handle_connection(server_sock, tmp_path, CaptureConfig())
    sender.join()

    store = Store.open(tmp_path / "recall.sqlite")
    try:
        events = store.capture_events_since(datetime.now(UTC) - timedelta(minutes=5))
    finally:
        store.close()
    kinds = [e.kind for e in events]
    assert capture_control.CaptureEventKind.INGEST_CONNECT in kinds
    disconnect = next(
        e
        for e in events
        if e.kind == capture_control.CaptureEventKind.INGEST_DISCONNECT
    )
    assert disconnect.source_id == "kitchen"
    assert disconnect.detail is not None
    stats = json.loads(disconnect.detail)
    assert stats["bytes"] == 96000  # every byte the device sent, counted
    assert stats["peak_db"] == -24.3  # 20*log10(2000/32768) — real signal, measured
    assert stats["first_audible_s"] == 0.0
    assert stats["first_byte_s"] is not None
    assert stats["flushed"].startswith("kitchen-")  # the close finalised a segment
    assert stats["flushed_bytes"] > 0


def _stream_once(tmp_path: Path, pcm: bytes) -> None:
    server_sock, client_sock = socket.socketpair()

    def device() -> None:
        client_sock.sendall(b'{"id":"kitchen"}\n')
        client_sock.sendall(pcm)
        client_sock.close()

    sender = threading.Thread(target=device)
    sender.start()
    handle_connection(server_sock, tmp_path, CaptureConfig())
    sender.join()


def test_alive_marker_needs_real_signal_not_just_bytes(tmp_path: Path) -> None:
    # "Active" must mean recording: a connected device streaming digital silence
    # (the pixel9 dead path, amplitude ~1) must NOT read as live — that green dot is
    # how speech got spoken into a not-recording window.
    _stream_once(tmp_path, b"\x01\x00" * 48000)
    assert not (tmp_path / "kitchen" / ".alive").exists()
    # Real signal (a live room's floor and above) refreshes the marker.
    _stream_once(tmp_path, (100).to_bytes(2, "little", signed=True) * 48000)
    assert (tmp_path / "kitchen" / ".alive").exists()


def test_handle_connection_segments_a_handshaked_stream(tmp_path: Path) -> None:
    server_sock, client_sock = socket.socketpair()

    # The device announces itself, streams ~1s of (silent) PCM, then disconnects —
    # from a thread, so the handler can read concurrently (else the socket buffer
    # fills and sendall blocks).
    def device() -> None:
        client_sock.sendall(b'{"id":"kitchen"}\n')
        client_sock.sendall(b"\x00\x00" * 48000)  # 1s @ 48k mono s16le
        client_sock.close()

    sender = threading.Thread(target=device)
    sender.start()
    handle_connection(server_sock, tmp_path, CaptureConfig())
    sender.join()

    # ffmpeg segmented the pumped stream — a file exists for the announced id.
    files = list((tmp_path / "kitchen").glob("kitchen-*"))
    assert files, "the handshaked stream should have been segmented to disk"
    # and the device auto-registered as a source by the id it announced.
    store = Store.open(tmp_path / "recall.sqlite")
    try:
        assert any(sid == "kitchen" for sid, _, _ in store.source_rows())
    finally:
        store.close()


def test_serve_listens_only_when_not_paused(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # A pause must stop phone recording, not just the USB mic: while paused the server
    # closes its listener, so connections are refused; on resume it listens again.
    port = _free_port()
    paused = {"v": True}
    monkeypatch.setattr(
        "recall.capture_control.is_paused", lambda _root, _now: paused["v"]
    )
    threading.Thread(target=serve, args=(tmp_path, port), daemon=True).start()
    time.sleep(0.4)  # let the loop run a few pause-checks

    with pytest.raises(OSError):  # paused -> no listener -> refused
        socket.create_connection(("127.0.0.1", port), timeout=1.0).close()

    paused["v"] = False  # resume -> the listener opens within a poll interval
    deadline = time.monotonic() + 5.0
    connected = False
    while time.monotonic() < deadline:
        try:
            socket.create_connection(("127.0.0.1", port), timeout=1.0).close()
            connected = True
            break
        except OSError:
            time.sleep(0.2)
    assert connected


class _ClosedMidStream:
    """A socket whose `recv` raises EBADF partway through, as a real one does
    when `serve` closes it from the pause loop.

    A `socketpair` cannot test this reliably: whether closing from another
    thread wakes a blocked `recv` or leaves it blocked is kernel behaviour, and
    the first attempt at this test hung for ten minutes on exactly that. The
    stub asserts what our code does with the error, which is the part we own.
    """

    def __init__(self, payload: bytes) -> None:
        self._buf = payload
        self.closed = False
        self.deadline: float | None = None

    def recv(self, n: int) -> bytes:
        # Honours `n`, because `read_handshake` reads ONE BYTE AT A TIME so it
        # cannot swallow any of the PCM behind the newline. A stub that ignored
        # the size handed it the whole line as a single non-newline "byte".
        if not self._buf:
            raise OSError(errno.EBADF, "Bad file descriptor")
        head, self._buf = self._buf[:n], self._buf[n:]
        return head

    def settimeout(self, seconds: float | None) -> None:
        self.deadline = seconds

    def close(self) -> None:
        self.closed = True


class _VanishedMidStream(_ClosedMidStream):
    """A socket whose `recv` TIMES OUT partway through, as a real one does once it
    has a deadline and the peer stopped sending without closing.

    Same reason for a stub as its parent: whether a kernel ever delivers anything
    for a half-open socket is not ours to control, and waiting 15 real seconds to
    find out is not a test.
    """

    @override
    def recv(self, n: int) -> bytes:
        if not self._buf:
            raise TimeoutError("timed out")
        head, self._buf = self._buf[:n], self._buf[n:]
        return head


def test_a_pause_dropping_the_stream_is_a_disconnect_not_a_crash(
    tmp_path: Path,
) -> None:
    """`serve` closes an active socket to stop a phone when capture pauses, and
    the reader learns by `recv` raising on the closed fd — that is the design.

    What it did with the exception was let it escape into the thread, so every
    pause printed a traceback: 168 of them in one log, none meaning anything,
    and a real one would have been indistinguishable. The audio was always fine
    (the `finally` still flushed), which is exactly why it went unnoticed.
    """
    sock = _ClosedMidStream(
        b'{"id":"kitchen"}\n' + (2000).to_bytes(2, "little", signed=True) * 4800
    )
    # The assertion is that this RETURNS rather than raising.
    handle_connection(cast("socket.socket", sock), tmp_path, CaptureConfig())
    assert sock.closed, "the handler left the socket open"

    store = Store.open(tmp_path / "recall.sqlite")
    try:
        events = store.capture_events_since(datetime.now(UTC) - timedelta(minutes=5))
    finally:
        store.close()
    disconnect = next(
        e
        for e in events
        if e.kind == capture_control.CaptureEventKind.INGEST_DISCONNECT
    )
    assert disconnect.detail is not None
    stats = json.loads(disconnect.detail)
    # And it says which of the two endings it was, which the record could not.
    assert "closed locally" in stats["ended"]
    assert stats["flushed"] is not None, "the pause lost the segment"


def _disconnect_stats(tmp_path: Path) -> dict[str, object]:
    store = Store.open(tmp_path / "recall.sqlite")
    try:
        events = store.capture_events_since(datetime.now(UTC) - timedelta(minutes=5))
    finally:
        store.close()
    disconnect = next(
        e
        for e in events
        if e.kind == capture_control.CaptureEventKind.INGEST_DISCONNECT
    )
    assert disconnect.detail is not None
    return cast("dict[str, object]", json.loads(disconnect.detail))


def test_a_phone_that_vanishes_is_recorded_as_gone_not_as_a_local_pause(
    tmp_path: Path,
) -> None:
    """A phone that dies without a FIN — reboot, force-quit, a Wi-Fi drop that ate
    the FIN — leaves a half-open socket. Without a deadline `recv` blocks forever:
    no ingest_disconnect is ever written and the source just goes quiet, which is
    indistinguishable from a quiet room because the liveness marker only tracks
    audible signal.

    The trap this pins: `TimeoutError` IS an `OSError`, so the pre-existing handler
    would have swallowed it and filed every dead phone under `closed locally` — the
    branch that means the household pause dropped the stream. Two opposite causes,
    one label, and the record is the only witness either leaves.
    """
    sock = _VanishedMidStream(
        b'{"id":"kitchen"}\n' + (2000).to_bytes(2, "little", signed=True) * 4800
    )
    handle_connection(cast("socket.socket", sock), tmp_path, CaptureConfig())
    assert sock.closed, "the handler left the socket open"

    stats = _disconnect_stats(tmp_path)
    ended = cast("str", stats["ended"])
    assert "closed locally" not in ended, "a vanished phone was filed as a local pause"
    assert "15s" in ended, f"the record does not say what happened: {ended!r}"
    # The audio the phone did send is still finalised, as on every other ending.
    assert stats["flushed"] is not None, "the timeout lost the segment"
    assert stats["bytes"] == 9600


def test_the_accepted_socket_carries_a_read_deadline(tmp_path: Path) -> None:
    """The deadline has to be on the SOCKET, not merely handled when it fires.

    Asserting only on the stub above would pass against a handler that never set
    one — the stub raises whatever it likes. This checks the real thing: a real
    socket, and what its timeout is by the time the pump is reading it.
    """
    server_sock, client_sock = socket.socketpair()
    seen: list[float | None] = []

    def device() -> None:
        client_sock.sendall(b'{"id":"kitchen"}\n')
        client_sock.sendall((2000).to_bytes(2, "little", signed=True) * 4800)
        # Read back by the handler before it can close; sample the deadline while
        # the connection is genuinely open.
        seen.append(server_sock.gettimeout())
        client_sock.close()

    sender = threading.Thread(target=device)
    sender.start()
    handle_connection(server_sock, tmp_path, CaptureConfig())
    sender.join()

    assert seen == [15.0], f"the accepted socket's deadline was {seen}"


def test_a_peer_that_never_speaks_does_not_hold_the_thread(tmp_path: Path) -> None:
    """The handshake read was unbounded too, and it runs before any of the pump's
    error handling exists. A peer that opens a connection and says nothing — a
    port scanner, a phone that died between connect and handshake — would hold a
    thread and a slot in `conns` for good, and it must not raise either: an
    escaping exception is the traceback flood the pause branch was written to end.
    """
    sock = _VanishedMidStream(b"")  # connects, then never sends a byte
    handle_connection(cast("socket.socket", sock), tmp_path, CaptureConfig())
    assert sock.closed, "the handler left the socket open"

    store = Store.open(tmp_path / "recall.sqlite")
    try:
        events = store.capture_events_since(datetime.now(UTC) - timedelta(minutes=5))
    finally:
        store.close()
    # No handshake means no source id, so there is nothing to file the connection
    # under — it must leave no half-formed record behind.
    assert events == []
