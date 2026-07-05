"""Single-port audio ingest server.

Every phone shares ONE port; the handshake carries identity, not the port. A device
opens a connection, sends a one-line handshake announcing its id and PCM format, then
streams raw PCM. The server reads only the handshake, then hands the socket to an
ffmpeg segmenter — so ffmpeg does all the audio, gap-free. The live connection is the
liveness signal, so there is no separate heartbeat.

This module starts with the handshake protocol (pure, fully tested); the accepting
server and the ffmpeg hand-off build on it.
"""

from __future__ import annotations

import json
import logging
import os
import re
import socket
import subprocess
import threading
import time
from collections.abc import Callable
from dataclasses import dataclass, replace
from datetime import UTC, datetime
from pathlib import Path

from recall import capture_control
from recall.capture import (
    CaptureConfig,
    build_segment_argv,
    container_ext,
    segment_output_pattern,
)
from recall.sources import AudioSource, SourceKind
from recall.store import Store

_log = logging.getLogger("recall.ingest")

# The one shared port every device connects to — hard-coded, so nothing needs setting
# on a phone but the host.
DEFAULT_INGEST_PORT = 9999

# A handshake id becomes a source id (and a directory name), so it must be safe.
_SAFE_ID = re.compile(r"^[a-z0-9][a-z0-9-]*$")
_DEFAULT_RATE = 48000
_DEFAULT_CHANNELS = 1
_MAX_HANDSHAKE_BYTES = 8192
_READ_CHUNK_BYTES = 65536  # socket -> ffmpeg pump chunk
_PAUSE_POLL_SECONDS = 2.0  # how often the accept loop re-checks the global pause

# Per-source liveness marker the server refreshes while a device streams; the API
# reads its freshness for the fleet view. (Replaces the phone-sent heartbeat.)
ALIVE_FILE = ".alive"


@dataclass(frozen=True)
class Handshake:
    """A device's opening announcement: who it is + its PCM format."""

    source_id: str
    sample_rate: int
    channels: int


def parse_handshake(line: str) -> Handshake | None:
    """Parse the handshake `{"id":"kitchen","rate":48000,"channels":1}`. rate/channels
    default to 48k mono. None if malformed or the id isn't filesystem-safe."""
    try:
        data = json.loads(line)
        source_id = str(data["id"])
    except (ValueError, KeyError, TypeError):
        return None
    if not _SAFE_ID.match(source_id):
        return None
    try:
        rate = int(data.get("rate", _DEFAULT_RATE))
        channels = int(data.get("channels", _DEFAULT_CHANNELS))
    except (ValueError, TypeError):
        return None
    return Handshake(source_id, rate, channels)


def read_handshake(
    recv: Callable[[int], bytes], *, max_bytes: int = _MAX_HANDSHAKE_BYTES
) -> Handshake | None:
    """Read exactly the newline-terminated handshake line from `recv`, byte by byte so
    not one byte of the PCM that follows is consumed. None on EOF, overflow, or a
    malformed line."""
    buf = bytearray()
    while len(buf) < max_bytes:
        chunk = recv(1)
        if not chunk:
            return None  # EOF before the newline
        if chunk == b"\n":
            return parse_handshake(buf.decode("utf-8", "replace"))
        buf += chunk
    return None  # never newline-terminated within the cap


def _register_source(root: Path, source_id: str) -> None:
    """Auto-register a device the first time it connects, by the id it announced.
    Idempotent; a friendly name can be set later in the UI."""
    store = Store.open(root / "recall.sqlite")
    try:
        store.register_source(
            AudioSource(id=source_id, name=source_id, kind=SourceKind.TCP_PCM, spec="")
        )
    finally:
        store.close()


def handle_connection(sock: socket.socket, root: Path, config: CaptureConfig) -> None:
    """Serve one device connection: read its handshake, register it, then pump its
    raw-PCM stream into an ffmpeg segmenter. The pump (socket -> ffmpeg's stdin pipe)
    is the one bit of Python in the path, but the kernel's TCP receive buffer absorbs
    any pause, so a momentary stall can't lose audio (the samples stay contiguous).
    Returns when the device disconnects."""
    try:
        handshake = read_handshake(sock.recv)
        if handshake is None:
            _log.warning("ingest: malformed handshake, dropping connection")
            return
        _register_source(root, handshake.source_id)
        seg = replace(
            config, sample_rate=handshake.sample_rate, channels=handshake.channels
        )
        out_dir = root / handshake.source_id
        out_dir.mkdir(parents=True, exist_ok=True)
        pattern = segment_output_pattern(
            str(root), handshake.source_id, ext=container_ext(seg.codec)
        )
        env = {**os.environ, "TZ": "UTC"}
        _log.info("ingest: %s connected", handshake.source_id)
        proc = subprocess.Popen(
            build_segment_argv(seg, pattern), stdin=subprocess.PIPE, env=env
        )
        if proc.stdin is None:  # pragma: no cover - PIPE always sets it
            msg = "ffmpeg stdin pipe was not created"
            raise RuntimeError(msg)
        # Liveness: the server owns the connection, so it reports the device live by
        # refreshing this file while data flows. /api/sources reads its freshness — no
        # phone-sent heartbeat needed any more.
        alive = out_dir / ALIVE_FILE
        try:
            while True:
                data = sock.recv(_READ_CHUNK_BYTES)
                if not data:
                    break  # the device disconnected
                proc.stdin.write(data)
                alive.touch()
        finally:
            proc.stdin.close()  # EOF -> ffmpeg finalises the current segment cleanly
            proc.wait()
        _log.info("ingest: %s disconnected", handshake.source_id)
    finally:
        sock.close()


def _open_listener(port: int) -> socket.socket:
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("0.0.0.0", port))
    srv.listen()
    srv.settimeout(_PAUSE_POLL_SECONDS)  # accept() returns to re-check the pause state
    return srv


def _serve_conn(
    conn: socket.socket, root: Path, config: CaptureConfig, conns: set[socket.socket]
) -> None:
    try:
        handle_connection(conn, root, config)
    finally:
        conns.discard(conn)


def serve(root: Path, port: int, *, config: CaptureConfig | None = None) -> None:
    """Accept device connections on one shared port; each announces itself in a
    handshake, then gets its own ffmpeg segmenter. Replaces the per-device ffmpeg
    listeners — the device id, not the port, is now the identity.

    Honours the global capture pause: while paused, the listener is closed (so phones
    are refused and back off) and any active stream is dropped — a pause stops phone
    recording just as it stops the USB mic.
    """
    config = config or CaptureConfig()
    conns: set[socket.socket] = set()
    listener: socket.socket | None = None
    try:
        while True:
            if capture_control.is_paused(root, datetime.now(UTC)):
                if listener is not None:
                    listener.close()
                    listener = None
                    for active in list(conns):
                        active.close()  # recv raises -> handler finalises + exits
                    _log.info("ingest: paused — not accepting")
                time.sleep(_PAUSE_POLL_SECONDS)
                continue
            if listener is None:
                listener = _open_listener(port)
                _log.info("ingest: listening on :%d", port)
            try:
                conn, _addr = listener.accept()
            except TimeoutError:
                continue  # accept timed out -> loop back to re-check the pause
            else:
                conns.add(conn)
                threading.Thread(
                    target=_serve_conn, args=(conn, root, config, conns), daemon=True
                ).start()
    finally:
        if listener is not None:
            listener.close()
