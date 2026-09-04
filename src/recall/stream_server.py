"""Single-port audio ingest server.

Every phone shares ONE port; the handshake carries identity, not the port. A device
opens a connection, sends a one-line handshake announcing its id and PCM format, then
streams raw PCM. The server reads only the handshake, then hands the socket to an
ffmpeg segmenter — so ffmpeg does all the audio, gap-free. The measured stream is the
liveness signal (the marker is refreshed only while real signal arrives), so there is
no separate heartbeat — and no way for a silent stream to read as recording.

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
from dataclasses import dataclass, field, replace
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import IO, NamedTuple

from recall import capture_control
from recall.capture import (
    SILENCE_PEAK,
    CaptureConfig,
    StreamMeter,
    build_segment_argv,
    container_ext,
    mark_alive,
    parse_segment_start,
    segment_glob,
    segment_output_pattern,
)
from recall.sources import AudioSource, SourceKind
from recall.store import Store

_log = logging.getLogger("recall.ingest")


# A handshake id becomes a source id (and a directory name), so it must be safe.
_SAFE_ID = re.compile(r"^[a-z0-9][a-z0-9-]*$")
_DEFAULT_RATE = 48000
_DEFAULT_CHANNELS = 1
_MAX_HANDSHAKE_BYTES = 8192
_READ_CHUNK_BYTES = 65536  # socket -> ffmpeg pump chunk
_PAUSE_POLL_SECONDS = 2.0  # how often the accept loop re-checks the global pause
# A mic stream is CONTINUOUS — 48 kHz * 2 bytes flows even in a silent room — so no
# data for this long means the peer is gone, and that is a far stronger signal than
# TCP keepalive, which never probes a connection the kernel still believes is fine.
# 15 s tolerates a brief Wi-Fi stall.
#
# This restores an intent the port dropped, rather than inventing one: the per-device
# ffmpeg listeners this module replaced passed the same 15 s as
# `tcp://...?listen=1&timeout=`, and `sources._TCP_READ_TIMEOUT_US` still carries it
# with the same reasoning in its comment. Without it a phone that vanishes without a
# FIN leaves `recv` blocked forever: no disconnect is ever recorded and the source
# simply goes quiet, which is indistinguishable from a quiet room.
_READ_TIMEOUT_SECONDS = 15.0


@dataclass(frozen=True)
class Handshake:
    """A device's opening announcement: who it is + its PCM format.

    `epoch` (optional) is the phone's wall-clock, in unix seconds, of the FIRST PCM
    byte it streams — what lets the server shift arrival-stamped segment names back
    to true capture time (#1332). None when the device doesn't send one (an older
    app) or sends garbage: a bad epoch degrades to arrival-stamping, never to a
    dropped stream — completeness outranks precision.
    """

    source_id: str
    sample_rate: int
    channels: int
    epoch: float | None = None


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
    try:
        raw_epoch = data.get("epoch")
        epoch = float(raw_epoch) if raw_epoch is not None else None
    except (ValueError, TypeError):
        epoch = None  # tolerated: see Handshake.epoch
    return Handshake(source_id, rate, channels, epoch)


# A phone clock this far from the server's is not synchronised at all (no NTP);
# applying it would smear segment names arbitrarily. Real buffering delays are
# seconds; real NTP skew is milliseconds.
_MAX_EPOCH_SKEW_S = 600.0
# How often the pump sweeps for closed segments to rebase. Cheap (one listdir),
# and well inside the worker's 120 s min-age indexing guard.
_REBASE_SWEEP_SECONDS = 10.0


def connection_offset(epoch: float | None, first_byte_wall: float) -> float | None:
    """Seconds to shift this connection's segment names: capture minus arrival.

    Negative is the physical case (the phone buffered before/while connecting, so
    the audio is OLDER than its arrival). A positive value can only be clock skew
    beating the buffering delay; renaming a segment FORWARD could pass ffmpeg's
    open segment — which liveness and the dead-segment watchdog identify as
    "the newest name" — so it clamps to 0.0 (arrival-stamping, today's exact
    behaviour). None when there is no epoch, or the epoch is so far from arrival
    (>_MAX_EPOCH_SKEW_S) that the phone's clock cannot be trusted at all.
    """
    if epoch is None:
        return None
    offset = epoch - first_byte_wall
    if abs(offset) > _MAX_EPOCH_SKEW_S:
        _log.warning(
            "ingest: epoch %.1fs away from arrival — phone clock untrusted, "
            "keeping arrival-stamped names",
            offset,
        )
        return None
    return min(offset, 0.0)


def rebase_segment_names(  # noqa: PLR0913 - the sweep's full write context, kept flat
    out_dir: Path,
    source_id: str,
    offset_s: float,
    done: set[str],
    *,
    since: datetime,
    include_newest: bool = False,
) -> list[tuple[str, str]]:
    """Shift every CLOSED segment of THIS connection by `offset_s`, renaming
    arrival time to capture time. The newest file is ffmpeg's open segment and is
    never touched (same rule as the dead-stub sweep); a file stamped before
    `since` (the connection's start) belongs to an earlier connection whose offset
    this one cannot know, and is left alone. `done` carries every name this
    connection has handled — including the names it CREATED, or the next sweep
    would shift a renamed file again and the archive would drift by the offset
    every sweep. A corrected name that already exists is KEPT under its arrival
    name forever — losing audio to a rename would invert priority #1. Returns the
    (old, new) renames performed.
    """
    shift = timedelta(seconds=round(offset_s))
    if not shift:
        return []
    renamed: list[tuple[str, str]] = []
    files = segment_glob(out_dir, source_id)
    if not include_newest:
        files = files[:-1]  # files[-1] is ffmpeg's open segment
    for path in files:
        if path.name in done:
            continue
        done.add(path.name)
        try:
            start = parse_segment_start(path.name)
        except ValueError:
            continue  # not a timestamped segment; leave it be
        if start < since:
            continue  # an earlier connection's segment; not ours to move
        corrected = start + shift
        new_name = f"{source_id}-{corrected:%Y%m%dT%H%M%S}{path.suffix}"
        if new_name == path.name:
            continue
        target = out_dir / new_name
        if target.exists():
            _log.warning(
                "ingest: %s keeps its arrival name — corrected slot %s is taken",
                path.name,
                new_name,
            )
            continue
        try:
            path.rename(target)
        except OSError as err:  # never let bookkeeping drop a stream
            _log.warning("ingest: could not rebase %s: %s", path.name, err)
            continue
        done.add(new_name)  # its own next sweep must not shift it again
        renamed.append((path.name, new_name))
    if renamed:
        _log.info(
            "ingest: %s rebased %d segment name(s) by %+.0fs to capture time",
            source_id,
            len(renamed),
            round(offset_s),
        )
    return renamed


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
    root: Path,
    kind: capture_control.CaptureEventKind,
    source_id: str,
    detail: str | None = None,
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


class _FlushedSegment(NamedTuple):
    name: str
    size: int


def _flushed_segment(
    out_dir: Path, source_id: str, *, since: float
) -> _FlushedSegment | None:
    """The segment file this connection last finalised: the newest one touched since
    the connection opened. None when the connection wrote no file at all — naming an
    older file would blame the wrong window."""
    newest: tuple[float, str, int] | None = None
    for path in segment_glob(out_dir, source_id):
        try:
            stat = path.stat()
        except OSError:
            continue
        if stat.st_mtime < since:
            continue
        if newest is None or (stat.st_mtime, path.name) > (newest[0], newest[1]):
            newest = (stat.st_mtime, path.name, stat.st_size)
    return _FlushedSegment(newest[1], newest[2]) if newest else None


def _accept_handshake(sock: socket.socket) -> Handshake | None:
    """The opening line, under the same deadline as the stream behind it.

    Bounded because the handshake read runs BEFORE the pump's error handling exists:
    a peer that connects and then says nothing — a port scanner, a phone that died
    between connect and handshake — would otherwise hold a thread and a slot in
    `conns` for good. Returns None for every way of not getting one, each logged
    with which way it was.
    """
    try:
        handshake = read_handshake(sock.recv)
    except TimeoutError:
        _log.warning("ingest: no handshake within %.0fs", _READ_TIMEOUT_SECONDS)
        return None
    if handshake is None:
        _log.warning("ingest: malformed handshake, dropping connection")
    return handshake


@dataclass
class _Pump:
    """The socket -> ffmpeg pump, and what it learned on the way.

    A dataclass rather than a function returning its findings, because the caller's
    `finally` files the disconnect record even when the pump exits by exception (a
    broken ffmpeg pipe, say) — and on that path `first_byte` is still evidence. A
    return value would be lost exactly when the record matters most.
    """

    sock: socket.socket
    stdin: IO[bytes]
    out_dir: Path
    meter: StreamMeter
    source_id: str
    #: The phone's announced capture epoch (Handshake.epoch); None = arrival-stamp.
    epoch: float | None = None
    #: Why the stream ended, for the disconnect record. The endings are not the
    #: same event and the log could not tell them apart: a phone walking out of
    #: range and a pause dropping the stream both just stopped.
    ended: str = "unknown"
    first_byte: float | None = None
    #: capture-minus-arrival for this connection, fixed at the first byte (#1332).
    offset_s: float | None = None
    #: Segment names already rebased (or created by a rebase) this connection.
    rebased: set[str] = field(default_factory=set)
    _next_sweep: float = 0.0

    def run(self) -> None:
        while True:
            try:
                data = self.sock.recv(_READ_CHUNK_BYTES)
            except TimeoutError:
                # MUST precede the OSError branch below: `TimeoutError` is a
                # subclass of it, so without this a phone that died mid-stream
                # would be filed as `closed locally` — the label that means the
                # household pause dropped the stream. Two opposite causes, and
                # this record is the only witness either of them leaves.
                self.ended = f"no data for {_READ_TIMEOUT_SECONDS:.0f}s — peer gone"
                _log.warning("ingest: %s stopped sending", self.source_id)
                return
            except OSError as exc:
                # `serve` closes an active socket to drop the stream when capture
                # pauses, and a recv on the closed fd is HOW the reader is told —
                # see the comment on that close. Expected, so it finalises like any
                # other disconnect instead of escaping into `_serve_conn` and
                # printing a thread traceback, which it did 168 times in this log
                # before 2026-08-10 without one of them meaning anything.
                self.ended = f"closed locally ({exc.strerror or exc})"
                return
            if not data:
                self.ended = "device disconnected"
                return
            self.stdin.write(data)  # the archive comes first; meter after
            if self.first_byte is None:
                self.first_byte = time.time()
                self.offset_s = connection_offset(self.epoch, self.first_byte)
            self._maybe_rebase()
            heard = self.meter.first_audible_s is not None
            # Liveness: refresh the marker only when the chunk carries real signal —
            # "active" must mean recording. A connected phone streaming digital
            # silence (the pixel9 dead path) reads idle, so nobody speaks trusting a
            # dot the audio can't back.
            if self.meter.feed(data) >= SILENCE_PEAK:
                mark_alive(self.out_dir)
            if not heard and self.meter.first_audible_s is not None:
                _log.info(
                    "ingest: %s first audible sample at %.2fs of stream",
                    self.source_id,
                    self.meter.first_audible_s,
                )

    def _maybe_rebase(self, *, final: bool = False) -> None:
        """Rename this connection's closed segments to capture time (#1332).

        Rides the pump loop (one listdir every _REBASE_SWEEP_SECONDS, well inside
        the worker's 120 s min-age indexing guard) rather than a thread — one
        fewer thing to stop. `final` runs once after ffmpeg has exited: every
        segment is closed then, so even the newest name is safe to move — without
        it the connection's LAST segment would stay arrival-stamped forever.
        """
        if self.offset_s is None or self.first_byte is None:
            return
        now = time.time()
        if not final and now < self._next_sweep:
            return
        self._next_sweep = now + _REBASE_SWEEP_SECONDS
        # 2 s slack: ffmpeg stamps the first segment by strftime (whole seconds),
        # which can floor to just before the measured first-byte instant. The
        # prior-connection exclusion only needs coarse precision — reconnects are
        # minutes apart, and a same-second collision is the EEXIST guard's job.
        since = datetime.fromtimestamp(self.first_byte - 2.0, tz=UTC)
        rebase_segment_names(
            self.out_dir,
            self.source_id,
            self.offset_s,
            self.rebased,
            since=since,
            include_newest=final,
        )


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
        # Bound every read on this socket, the handshake included.
        sock.settimeout(_READ_TIMEOUT_SECONDS)
        handshake = _accept_handshake(sock)
        if handshake is None:
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
        _record_event(
            root, capture_control.CaptureEventKind.INGEST_CONNECT, handshake.source_id
        )
        connected = time.time()
        meter = StreamMeter(handshake.sample_rate, handshake.channels)
        proc = subprocess.Popen(
            build_segment_argv(seg, pattern), stdin=subprocess.PIPE, env=env
        )
        if proc.stdin is None:  # pragma: no cover - PIPE always sets it
            msg = "ffmpeg stdin pipe was not created"
            raise RuntimeError(msg)
        pump = _Pump(
            sock=sock,
            stdin=proc.stdin,
            out_dir=out_dir,
            meter=meter,
            source_id=handshake.source_id,
            epoch=handshake.epoch,
        )
        try:
            pump.run()
        finally:
            ended, first_byte = pump.ended, pump.first_byte
            proc.stdin.close()  # EOF -> ffmpeg finalises the current segment cleanly
            proc.wait()
            flushed = _flushed_segment(out_dir, handshake.source_id, since=connected)
            # Every segment is closed now: give the connection's LAST one its
            # capture-time name too. After _flushed_segment, whose stats keep
            # ffmpeg's own (arrival) name for the disconnect record.
            pump._maybe_rebase(final=True)
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
                    "flushed": flushed.name if flushed else None,
                    "flushed_bytes": flushed.size if flushed else None,
                    "ended": ended,
                }
            )
            _record_event(
                root,
                capture_control.CaptureEventKind.INGEST_DISCONNECT,
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
