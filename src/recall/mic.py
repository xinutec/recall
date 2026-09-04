"""Linux mic client — an always-on host streams its ALSA microphone to the ingester.

The phones carry a whole app to do this because they roam, sleep, and must be told
by hand to come back. A mains-powered Linux box needs none of that: what is left is
the wire protocol, the spool discipline, and a process supervisor. This module is
those first two; systemd is the third.

**The same protocol as the phones**, deliberately (`docs/devices.md`): connect to the
one shared ingest port, send a one-line handshake announcing id/rate/channels/epoch,
then stream raw s16le PCM. The server auto-registers the source, so a new Linux mic
needs no host-side provisioning either.

**ffmpeg owns the audio path.** It opens ALSA, downmixes, and writes mono PCM to a
pipe; this module only moves bytes from that pipe to a socket. That split is the same
one `docs/devices.md` draws for the USB mic — our code stays out of the part where a
stall costs samples.

**Why a spool at all, on a box with a perfect link.** A TCP source cannot be pumped
straight from device to socket: when the network stalls, the send blocks, the pipe
fills, and ALSA overruns — the phone-side failure that cost real audio (#1330's
sibling). So capture hands bytes to a bounded ring and returns immediately, and a
separate drain does the sending. Audio is dropped only at the ring, only when the
ring is full, and never without being counted.

**Why the drain discards while disconnected**, rather than banking a backlog the box
has ample RAM to hold: the server rebases a connection's segment names by ONE offset,
measured at its first byte (`stream_server.connection_offset`). A replayed backlog
would be stamped correctly at its head and increasingly wrong toward its tail, so
holding audio across a disconnect needs a protocol that times each segment, not a
bigger buffer here.
"""

from __future__ import annotations

import json
import logging
import socket
import subprocess
import threading
import time
from dataclasses import dataclass

from recall.wire import BYTES_PER_SAMPLE, DEFAULT_INGEST_PORT, SAMPLE_RATE

_log = logging.getLogger("recall.mic")

# 60 s of mono audio, matching the phones' spool so one number describes the fleet.
# It is a backpressure cushion, not a store: see the module docstring on why a
# bigger one would not buy a disconnect.
DEFAULT_SPOOL_SECONDS = 60

# ~43 ms per read. Small enough that a read is never a latency source, large enough
# that the loop is not syscall-bound.
_READ_CHUNK_BYTES = 4096

# ffmpeg's own input queue, in packets — slack for a scheduling hiccup on this side
# before ALSA is the thing that overruns.
_THREAD_QUEUE_SIZE = 1024

_CONNECT_TIMEOUT_S = 5.0
_RECONNECT_DELAY_S = 2.0
# Nothing to send: how long the drain sleeps before looking again. Well under the
# spool's depth, so it can never be the reason the ring fills.
_IDLE_POLL_S = 0.02


class PcmSpool:
    """Bounded capture-to-network hand-off: `offer` never blocks, and drops the
    OLDEST audio when full.

    Drop-oldest rather than drop-newest because the newest audio is the audio
    somebody is speaking now. Drops are counted, never silent — bytes lost here are
    invisible at every later stage, so this counter is the only evidence they
    existed (the same reasoning as the phones' `PcmSpool`).
    """

    def __init__(self, capacity_bytes: int) -> None:
        if capacity_bytes <= 0:
            raise ValueError("capacity_bytes must be positive")
        self._capacity = capacity_bytes
        self._buf = bytearray()
        self._lock = threading.Lock()
        self._dropped = 0

    @property
    def dropped(self) -> int:
        """Total bytes discarded because the ring was full."""
        with self._lock:
            return self._dropped

    def offer(self, data: bytes) -> None:
        """Add captured audio. Never blocks, never raises — the capture thread's
        only contract is that it returns."""
        if not data:
            return
        with self._lock:
            self._buf += data
            excess = len(self._buf) - self._capacity
            if excess > 0:
                del self._buf[:excess]
                self._dropped += excess

    def drain(self) -> bytes:
        """Take everything spooled so far, oldest first. Empty when there is none."""
        with self._lock:
            if not self._buf:
                return b""
            out = bytes(self._buf)
            self._buf.clear()
            return out


class ConnectionNarrator:
    """Decides which connection outcomes are worth a log line.

    While the household is paused the ingest listener is CLOSED, so a correct client
    fails to connect every 2 s for as long as the pause lasts — routinely days. One
    line per attempt would be ~43,000 a day and would bury the reconnection, which is
    the line somebody chasing a gap in the archive actually needs. So a run of
    failures reports once, and each transition reports again.
    """

    def __init__(self) -> None:
        self._reported_failure = False

    def should_report_failure(self) -> bool:
        """True for the FIRST failure of a run of them."""
        if self._reported_failure:
            return False
        self._reported_failure = True
        return True

    def note_connected(self) -> None:
        """Re-arm: the next failure starts a new run, and is worth saying."""
        self._reported_failure = False


def handshake_line(source_id: str, *, rate: int, channels: int, epoch: float) -> bytes:
    """The one-line announcement the ingest server reads before any PCM.

    `epoch` is the wall-clock of the first PCM byte this connection will carry, which
    lets the server rename arrival-stamped segments back to capture time. Fixed-point,
    like both phone clients: the server's `float()` would accept exponent form, but a
    wire format shared by three clients is worth more than the tolerance.
    """
    body = json.dumps(
        {"id": source_id, "rate": rate, "channels": channels, "epoch": round(epoch, 3)}
    )
    return body.encode() + b"\n"


def capture_argv(device: str, *, input_rate: int, input_channels: int) -> list[str]:
    """ffmpeg argv: open `device` via ALSA, downmix to mono s16le on stdout.

    The input shape is stated rather than inferred. The mic this was written for
    (a USB conference unit) offers capture at 48 kHz stereo and nothing else, and
    its two channels are bit-identical — a single capsule presented as a pair — so
    the downmix costs nothing and halves the bytes on the wire. Naming the shape
    means a device that stops offering it fails loudly instead of being quietly
    resampled into a format nobody chose.
    """
    return [
        "ffmpeg",
        "-hide_banner",
        "-loglevel",
        "warning",
        "-f",
        "alsa",
        "-ac",
        str(input_channels),
        "-ar",
        str(input_rate),
        "-thread_queue_size",
        str(_THREAD_QUEUE_SIZE),
        "-i",
        device,
        "-ac",
        "1",
        "-f",
        "s16le",
        "-",
    ]


@dataclass(frozen=True)
class MicConfig:
    """Everything this client needs. No secrets: the ingest port has no auth, by
    design (`docs/devices.md` — a mic that could 401 reports a credential mistake
    as dead hardware)."""

    source_id: str
    host: str
    device: str = "default"
    port: int = DEFAULT_INGEST_PORT
    input_rate: int = SAMPLE_RATE
    input_channels: int = 2
    spool_seconds: int = DEFAULT_SPOOL_SECONDS


def _spool_capacity(config: MicConfig) -> int:
    """Spool size in bytes, derived from the rate rather than written as a literal:
    a hardcoded byte count silently made the iOS spool a THIRD of its intended
    depth against a 48 kHz stream (d983f96)."""
    return SAMPLE_RATE * BYTES_PER_SAMPLE * config.spool_seconds


def _pump_capture(stdout: object, spool: PcmSpool, stop: threading.Event) -> None:
    """Move captured bytes into the spool until ffmpeg ends or we are told to stop.

    Deliberately the shortest loop in the module: everything it does not do is
    something that cannot delay the microphone.
    """
    read = getattr(stdout, "read", None)
    if read is None:  # pragma: no cover - a closed pipe, not a reachable state
        return
    while not stop.is_set():
        chunk = read(_READ_CHUNK_BYTES)
        if not chunk:
            return  # ffmpeg exited; the supervisor above restarts the process
        spool.offer(chunk)


def _connect(host: str, port: int) -> socket.socket | None:
    """One connection attempt. None on any failure — an unreachable recorder is the
    normal state while the household is paused, not an error to raise. Reporting is
    the caller's, via ConnectionNarrator."""
    try:
        sock = socket.create_connection((host, port), timeout=_CONNECT_TIMEOUT_S)
    except OSError:
        return None
    sock.settimeout(None)
    sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
    return sock


def _stream(sock: socket.socket, spool: PcmSpool, stop: threading.Event) -> None:
    """Send spooled audio until the connection fails or we are told to stop."""
    reported_drops = 0
    while not stop.is_set():
        pending = spool.drain()
        if not pending:
            time.sleep(_IDLE_POLL_S)
            continue
        sock.sendall(pending)
        dropped = spool.dropped
        if dropped > reported_drops:
            _log.warning(
                "mic: spool overran — %d bytes (%.1fs) of audio dropped in total",
                dropped,
                dropped / (SAMPLE_RATE * BYTES_PER_SAMPLE),
            )
            reported_drops = dropped


def run(config: MicConfig, *, stop: threading.Event | None = None) -> int:
    """Capture and stream until `stop` is set or the capture process exits.

    Returns a process exit code. A dead ffmpeg is reported by EXITING rather than
    by restarting it here: systemd already supervises this unit, and one restart
    path is easier to reason about than two.
    """
    stop = threading.Event() if stop is None else stop
    spool = PcmSpool(_spool_capacity(config))
    narrator = ConnectionNarrator()
    argv = capture_argv(
        config.device,
        input_rate=config.input_rate,
        input_channels=config.input_channels,
    )
    _log.info(
        "mic: %s -> %s:%d as '%s'",
        config.device,
        config.host,
        config.port,
        config.source_id,
    )
    proc = subprocess.Popen(argv, stdout=subprocess.PIPE)
    pump = threading.Thread(
        target=_pump_capture,
        args=(proc.stdout, spool, stop),
        daemon=True,
        name="mic-capture",
    )
    pump.start()
    try:
        while not stop.is_set():
            if proc.poll() is not None:
                _log.error(
                    "mic: capture exited with %s — letting the supervisor restart",
                    proc.returncode,
                )
                return 1
            sock = _connect(config.host, config.port)
            if sock is None:
                if narrator.should_report_failure():
                    _log.info(
                        "mic: %s:%d unreachable — retrying every %.0fs (a paused "
                        "household closes the listener; silent until it returns)",
                        config.host,
                        config.port,
                        _RECONNECT_DELAY_S,
                    )
                # Discard whatever spooled while unreachable: see the module
                # docstring — a banked backlog cannot be timestamped correctly.
                spool.drain()
                stop.wait(_RECONNECT_DELAY_S)
                continue
            with sock:
                try:
                    # Drop the pre-connection spool BEFORE stamping the epoch, so
                    # the first byte sent really was captured at the time claimed.
                    spool.drain()
                    sock.sendall(
                        handshake_line(
                            config.source_id,
                            rate=SAMPLE_RATE,
                            channels=1,
                            epoch=time.time(),
                        )
                    )
                    narrator.note_connected()
                    _log.info("mic: streaming to %s:%d", config.host, config.port)
                    _stream(sock, spool, stop)
                except OSError as err:
                    _log.info("mic: connection ended (%s)", err)
            stop.wait(_RECONNECT_DELAY_S)
    finally:
        stop.set()
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:  # pragma: no cover - a wedged ffmpeg
            proc.kill()
    return 0
