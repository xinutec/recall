"""Single-port audio ingest: the opening handshake by which a device announces
itself, so many devices share one port instead of one ffmpeg listener each."""

from __future__ import annotations

import socket
import threading
import time
from pathlib import Path

import pytest

from recall.capture import CaptureConfig
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
