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
import math
import os
import re
import socket
import subprocess
import threading
import time
from array import array
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

# |s16 sample| at/above this counts as signal (~ -66 dBFS): safely above digital
# silence and codec dither (the pixel9 dead-path bug streams at amplitude ~1, -90 dB),
# safely below any live mic in a quiet room (~ -60..-50 dB).
_AUDIBLE_FLOOR = 16
_S16_FULL_SCALE = 32768


class StreamMeter:
    """Measures a raw s16le PCM stream as it is pumped, so a connection leaves
    evidence of what the device actually sent (docs/capture-loss-plan.md Phase 1):
    total bytes, peak level, and when the first *audible* sample arrived — in stream
    time, so the phone's wall clock can't confuse it. Chunks need not respect sample
    boundaries; a half sample carries to the next feed."""

    def __init__(self, sample_rate: int, channels: int) -> None:
        self._byte_rate = 2 * sample_rate * channels  # s16 = 2 bytes/sample
        self._carry = b""
        self.bytes_total = 0
        self.peak = 0
        self.first_audible_byte: int | None = None

    def feed(self, data: bytes) -> None:
        start = self.bytes_total - len(self._carry)  # stream offset of buf[0]
        self.bytes_total += len(data)
        buf = self._carry + data
        if len(buf) % 2:
            self._carry = buf[-1:]
            buf = buf[:-1]
        else:
            self._carry = b""
        if not buf:
            return
        # array('h') reads native-endian shorts == little-endian s16 on every host
        # recall runs on (arm64/x86_64).
        samples = array("h", buf)
        low, high = min(samples), max(samples)
        peak = max(high, -low)
        self.peak = max(self.peak, peak)
        if self.first_audible_byte is None and peak >= _AUDIBLE_FLOOR:
            index = next(i for i, s in enumerate(samples) if abs(s) >= _AUDIBLE_FLOOR)
            self.first_audible_byte = start + 2 * index

    @property
    def peak_db(self) -> float | None:
        """Loudest sample seen, in dBFS; None when not one non-zero sample arrived
        (pure digital zeros — indistinguishable from no capture path at all)."""
        if self.peak == 0:
            return None
        return round(20 * math.log10(self.peak / _S16_FULL_SCALE), 1)

    @property
    def first_audible_s(self) -> float | None:
        """Stream-time seconds until the first sample at/above the audible floor;
        None when the whole stream stayed below it (silence)."""
        if self.first_audible_byte is None:
            return None
        return self.first_audible_byte / self._byte_rate


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


def _record_event(
    root: Path, kind: str, source_id: str, detail: str | None = None
) -> None:
    """Durable telemetry row (capture_events). Best-effort: the audio pump must never
    stall or die over bookkeeping, so a failure is logged and swallowed."""
    try:
        store = Store.open(root / "recall.sqlite")
        try:
            store.add_capture_event(
                kind, utc=datetime.now(UTC), source_id=source_id, detail=detail
            )
        finally:
            store.close()
    except Exception:
        _log.exception("ingest: could not record %s for %s", kind, source_id)


def _flushed_segment(
    out_dir: Path, source_id: str, *, since: float
) -> tuple[str, int] | None:
    """The segment file this connection last finalised: the newest one touched since
    the connection opened. None when the connection wrote no file at all — naming an
    older file would blame the wrong window."""
    newest: tuple[float, str, int] | None = None
    for path in out_dir.glob(f"{source_id}-*"):
        try:
            stat = path.stat()
        except OSError:
            continue
        if stat.st_mtime < since:
            continue
        if newest is None or (stat.st_mtime, path.name) > (newest[0], newest[1]):
            newest = (stat.st_mtime, path.name, stat.st_size)
    return (newest[1], newest[2]) if newest else None


def handle_connection(sock: socket.socket, root: Path, config: CaptureConfig) -> None:
    """Serve one device connection: read its handshake, register it, then pump its
    raw-PCM stream into an ffmpeg segmenter. The pump (socket -> ffmpeg's stdin pipe)
    is the one bit of Python in the path, but the kernel's TCP receive buffer absorbs
    any pause, so a momentary stall can't lose audio (the samples stay contiguous).
    Returns when the device disconnects.

    Every connection leaves durable evidence (capture_events): an ingest_connect on
    open, and an ingest_disconnect on close carrying what the device actually sent —
    bytes, measured peak level, time to the first byte and to the first *audible*
    sample, and which segment file the close flushed. That record is what tells a
    stream of digital silence from no stream at all when speech goes missing."""
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
        _record_event(root, capture_control.EVENT_INGEST_CONNECT, handshake.source_id)
        connected = time.time()
        meter = StreamMeter(handshake.sample_rate, handshake.channels)
        first_byte: float | None = None
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
                proc.stdin.write(data)  # the archive comes first; meter after
                alive.touch()
                if first_byte is None:
                    first_byte = time.time()
                heard = meter.first_audible_s is not None
                meter.feed(data)
                if not heard and meter.first_audible_s is not None:
                    _log.info(
                        "ingest: %s first audible sample at %.2fs of stream",
                        handshake.source_id,
                        meter.first_audible_s,
                    )
        finally:
            proc.stdin.close()  # EOF -> ffmpeg finalises the current segment cleanly
            proc.wait()
            flushed = _flushed_segment(out_dir, handshake.source_id, since=connected)
            stats = json.dumps(
                {
                    "seconds": round(time.time() - connected, 1),
                    "bytes": meter.bytes_total,
                    "peak_db": meter.peak_db,
                    "first_byte_s": (
                        round(first_byte - connected, 2)
                        if first_byte is not None
                        else None
                    ),
                    "first_audible_s": (
                        round(meter.first_audible_s, 2)
                        if meter.first_audible_s is not None
                        else None
                    ),
                    "flushed": flushed[0] if flushed else None,
                    "flushed_bytes": flushed[1] if flushed else None,
                }
            )
            _record_event(
                root,
                capture_control.EVENT_INGEST_DISCONNECT,
                handshake.source_id,
                stats,
            )
            _log.info("ingest: %s disconnected — %s", handshake.source_id, stats)
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
