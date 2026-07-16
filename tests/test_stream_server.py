"""Single-port audio ingest: the opening handshake by which a device announces
itself, so many devices share one port instead of one ffmpeg listener each."""

from __future__ import annotations

import json
import socket
import threading
import time
from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest

from recall import capture_control
from recall.capture import CaptureConfig
from recall.store import Store
from recall.stream_server import (
    Handshake,
    StreamMeter,
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
    # The Phase-1 evidence (docs/capture-loss-plan.md): a connection leaves a durable
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
    # how speech got spoken into a not-recording window (docs/capture-loss-plan.md).
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
