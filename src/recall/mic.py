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

import argparse
import json
import logging
import socket
import subprocess
import threading
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path

from recall.beat_relay import DEFAULT_FLEET_URL, DEFAULT_RELAY_PORT
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

    @property
    def pending(self) -> int:
        """Bytes spooled but not yet taken."""
        with self._lock:
            return len(self._buf)

    def drain(self) -> bytes:
        """Take everything spooled so far, oldest first. Empty when there is none."""
        with self._lock:
            if not self._buf:
                return b""
            out = bytes(self._buf)
            self._buf.clear()
            return out


# Hourly, matching `mic_alive.BEAT_EVERY_MINUTES`. The fleet's thresholds are
# written as multiples of that constant, so the two must not drift.
BEAT_EVERY_S = 3600
# A first beat that failed comes back in a minute, doubling to the hourly cadence.
# A recorder that came up while the network was still settling used to wait a full
# hour to be believed (#886).
_BEAT_RETRY_START_S = 60
_BEAT_TIMEOUT_S = 8.0
# 2xx is delivered; anything else is not, including a redirect we will not follow.
_HTTP_OK = 200
_HTTP_REDIRECT = 300


def beat_backoff(consecutive_failures: int) -> int:
    """Seconds to wait before the next beat attempt."""
    return int(min(_BEAT_RETRY_START_S * 2**consecutive_failures, BEAT_EVERY_S))


def build_version() -> str:
    """Which build this is, for the reader of a beat that stopped arriving.

    On a deployed box this module lives at /nix/store/<hash>-source/src/recall/, and
    that hash IS the identity of the deploy — so a restart into a new build is
    legible rather than looking like the same unit flapping. Empty when running from
    a checkout, where the answer would be a guess.
    """
    for part in Path(__file__).resolve().parts:
        if "-" in part and part.endswith("-source"):
            return part.split("-", 1)[0][:12]
    return ""


def beat_payload(
    source_id: str,
    *,
    started_at: str | None,
    streaming: bool,
    mic_ok: bool,
    version: str,
) -> dict[str, object]:
    """The hourly "I am still here" (#837).

    `charging` is deliberately absent: the field renders as "on battery" when false,
    and a mains-powered box has no such state to report either way.
    """
    payload: dict[str, object] = {
        "device": source_id,
        "app": "linux",
        "version": version,
        "streaming": streaming,
        "micOk": mic_ok,
    }
    if started_at is not None:
        payload["startedAt"] = started_at
    return payload


def post_beat(
    base_url: str, payload: dict[str, object], *, timeout: float = _BEAT_TIMEOUT_S
) -> bool:
    """Send one beat. True if it was accepted.

    Best-effort and quiet: a liveness report that raised its own failures would be
    the tail wagging the dog, and half this fleet is unreachable from here at any
    given moment. Unauthenticated on purpose — the beat plane is device-exempt, so a
    beat that could 401 would report a credential mistake as dead hardware.
    """
    request = urllib.request.Request(
        f"{base_url.rstrip('/')}/api/devices/heartbeat",
        data=json.dumps(payload).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            status = int(response.status)
            return _HTTP_OK <= status < _HTTP_REDIRECT
    except (urllib.error.URLError, OSError, ValueError):
        return False


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
        # ffmpeg reads stdin for interactive commands. Under systemd stdin is
        # /dev/null and this changes nothing; run by hand from a shell script it
        # is the difference between the script finishing and ffmpeg swallowing
        # the rest of it. Belt and braces with the DEVNULL below.
        "-nostdin",
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
    # Where the hourly beat goes FIRST — the fleet's control plane, which is the
    # only place a beat lives. Empty disables the beat entirely.
    control_url: str = DEFAULT_FLEET_URL
    # And where it goes when the control plane cannot be reached: `recall beat-relay`
    # on the recorder host's LAN port, which forwards it and stamps `viaLan` itself.
    # Same two-tier fallback the phones use, so a route that starts working again
    # silently stops being reported as the back road.
    relay_port: int = DEFAULT_RELAY_PORT
    input_rate: int = SAMPLE_RATE
    input_channels: int = 2
    spool_seconds: int = DEFAULT_SPOOL_SECONDS


def _spool_capacity(config: MicConfig) -> int:
    """Spool size in bytes, derived from the rate rather than written as a literal:
    a hardcoded byte count silently made the iOS spool a THIRD of its intended
    depth against a 48 kHz stream (d983f96)."""
    return SAMPLE_RATE * BYTES_PER_SAMPLE * config.spool_seconds


def _pump_capture(
    stdout: object, spool: PcmSpool, stop: threading.Event, ended: threading.Event
) -> None:
    """Move captured bytes into the spool until ffmpeg ends or we are told to stop.

    Deliberately the shortest loop in the module: everything it does not do is
    something that cannot delay the microphone. Setting `ended` on EOF is the one
    exception, and it earns its place — see `_stream`.
    """
    try:
        read = getattr(stdout, "read", None)
        if read is None:  # pragma: no cover - a closed pipe, not a reachable state
            return
        while not stop.is_set():
            chunk = read(_READ_CHUNK_BYTES)
            if not chunk:
                return  # ffmpeg exited; the supervisor above restarts the process
            spool.offer(chunk)
    finally:
        ended.set()


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


def _stream(
    sock: socket.socket,
    spool: PcmSpool,
    stop: threading.Event,
    capture_ended: threading.Event,
) -> None:
    """Send spooled audio until capture ends, the connection fails, or we stop.

    `capture_ended` is not redundant with a dead connection. A capture process that
    dies while the socket stays healthy would otherwise leave this loop sending
    nothing for ever: the server would keep the connection open, stop refreshing the
    source's liveness marker, and the mic would read as a quiet room rather than a
    broken one. Ending here surfaces it as a restart instead.
    """
    reported_drops = 0
    while not stop.is_set():
        if capture_ended.is_set() and not spool.pending:
            _log.error("mic: capture ended while connected — ending the stream")
            return
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


def _beat_forever(
    config: MicConfig,
    *,
    connected: threading.Event,
    capture_ended: threading.Event,
    stop: threading.Event,
) -> None:
    """Say "still here" hourly, whether or not anything is streaming.

    That is the whole point: the source's liveness marker is refreshed only by audio
    above the silence floor, and while the household is paused the ingest listener is
    closed and nothing streams at all — which is exactly the window in which a dead
    recorder goes unnoticed.
    """
    started_at = datetime.now(UTC).strftime("%Y-%m-%dT%H:%M:%SZ")
    version = build_version()
    targets = [
        t
        for t in (config.control_url, f"http://{config.host}:{config.relay_port}")
        if t
    ]
    failures = 0
    while not stop.is_set():
        payload = beat_payload(
            config.source_id,
            started_at=started_at,
            streaming=connected.is_set(),
            mic_ok=not capture_ended.is_set(),
            version=version,
        )
        if any(post_beat(target, payload) for target in targets):
            failures = 0
            stop.wait(BEAT_EVERY_S)
            continue
        failures += 1
        if failures == 1:
            _log.info("mic: beat undelivered — retrying, first at %ds", beat_backoff(0))
        stop.wait(beat_backoff(failures - 1))


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
    capture_ended = threading.Event()
    connected = threading.Event()
    proc = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE)
    pump = threading.Thread(
        target=_pump_capture,
        args=(proc.stdout, spool, stop, capture_ended),
        daemon=True,
        name="mic-capture",
    )
    pump.start()
    threading.Thread(
        target=_beat_forever,
        kwargs={
            "config": config,
            "connected": connected,
            "capture_ended": capture_ended,
            "stop": stop,
        },
        daemon=True,
        name="mic-beat",
    ).start()
    try:
        while not stop.is_set():
            if capture_ended.is_set() or proc.poll() is not None:
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
                    connected.set()
                    _log.info("mic: streaming to %s:%d", config.host, config.port)
                    _stream(sock, spool, stop, capture_ended)
                except OSError as err:
                    _log.info("mic: connection ended (%s)", err)
                finally:
                    connected.clear()
            stop.wait(_RECONNECT_DELAY_S)
    finally:
        stop.set()
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:  # pragma: no cover - a wedged ffmpeg
            proc.kill()
    return 0


def parse_args(argv: list[str] | None = None) -> MicConfig:
    """The deployment's own command line.

    Separate from the main `recall` CLI, and deliberately not a subcommand of it:
    that parser imports the store, the web stack and the ML stack, none of which a
    box whose only job is a microphone has any reason to install. It would also
    offer `recall mic` on macOS, where there is no ALSA to open — a command listed
    exactly where it cannot run.
    """
    parser = argparse.ArgumentParser(
        prog="python -m recall.mic",
        description="Stream this Linux host's microphone to a recall ingest server.",
    )
    parser.add_argument("--id", required=True, help="source id (filesystem-safe)")
    parser.add_argument(
        "--host",
        required=True,
        help="host running `recall ingest` (a name, not an address)",
    )
    parser.add_argument("--port", type=int, default=DEFAULT_INGEST_PORT)
    parser.add_argument(
        "--device",
        default="default",
        help="ALSA capture device. Prefer hw:CARD=<id>,DEV=0 over hw:N,0 — card "
        "numbers follow enumeration order and can move capture to another input",
    )
    parser.add_argument(
        "--input-channels",
        type=int,
        default=2,
        help="channels the DEVICE offers; always downmixed to mono on the wire",
    )
    parser.add_argument("--input-rate", type=int, default=SAMPLE_RATE)
    parser.add_argument("--spool-seconds", type=int, default=DEFAULT_SPOOL_SECONDS)
    parser.add_argument(
        "--control-url",
        default=DEFAULT_FLEET_URL,
        help="fleet control plane for the hourly beat; empty disables beating",
    )
    parser.add_argument(
        "--relay-port",
        type=int,
        default=DEFAULT_RELAY_PORT,
        help="`recall beat-relay` port on --host, used when the control plane "
        "cannot be reached",
    )
    args = parser.parse_args(argv)
    return MicConfig(
        source_id=args.id,
        host=args.host,
        port=args.port,
        device=args.device,
        input_rate=args.input_rate,
        input_channels=args.input_channels,
        spool_seconds=args.spool_seconds,
        control_url=args.control_url,
        relay_port=args.relay_port,
    )


def main(argv: list[str] | None = None) -> int:
    """Entry point. Logs on the same UTC clock as the ingest server it talks to, so
    the two sides' timelines can be read together."""
    logging.Formatter.converter = time.gmtime
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)sZ %(name)s: %(message)s",
        datefmt="%Y-%m-%dT%H:%M:%S",
    )
    return run(parse_args(argv))


if __name__ == "__main__":
    raise SystemExit(main())
