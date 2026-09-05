"""LAN fallback for the mic heartbeat: the Mac accepts a beat and passes it to the
fleet (#888).

WHY THIS EXISTS. The beat's reachability requirement used to be STRICTER than
recording's, which made the check lie about working phones:

    audio   phone -> Mac,  192.168.1.81:9999   (LAN)
    beat    phone -> Isis, 10.100.0.2:8000     (VPN)

So a phone sitting at home with its tunnel off streamed every sample correctly and
still read `silent` after 3 h and `dead` after 12 h. Both false — the mic was
working. That is the error #837 was built to remove (a quiet room and a dead app
reading alike), reintroduced one layer up. Measured twice on 2026-08-14, when the
iPhone's WireGuard was off and `devicectl` kept reaching it over the LAN the whole
time, which is exactly what made it look like an app fault.

WHY IT IS NOT THE INGEST PORT. The obvious home for this is the port the phones
already talk to, but the ingest server CLOSES its listener while capture is
paused — and a pause is precisely when the heartbeat is the only signal there is.
A beat receiver has to be independent of the capture lifecycle, so it is its own
tiny server that runs whatever capture is doing.

WHY PORT 8000. The same port the fleet API answers on, so the apps need no second
URL shape: the fallback is the identical request with `host` swapped for
`controlHost`. It is a different machine, so nothing collides.

⚠ ISIS REMAINS THE SINGLE SOURCE OF TRUTH. This forwards; it does not store. A
local beat store the collector had to merge with the fleet's would let two places
disagree about which mics are alive, which is worse than the bug being fixed.

⚠ UNAUTHENTICATED, like the endpoint it forwards to — a mic app has never held a
credential, and a beat that could 401 would report a credential mistake as dead
hardware. But this one listens on the LAN rather than behind the VPN, so it
forwards an ALLOWLIST of known fields and stamps `viaLan` itself: a caller here
can lie about its own state (as any phone always could) but cannot inject fields
into the fleet store or deny that it came the back way.

Stdlib only, deliberately: this runs on the Mac beside the capture agent, and the
import-surface checks keep that process free of the API's dependencies.
"""

from __future__ import annotations

import json
import logging
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any, Final, override

_log = logging.getLogger("recall.beat_relay")

#: Same port as the fleet API, on a different machine — see the module docstring.
DEFAULT_RELAY_PORT: Final = 8000

#: Where a relayed beat goes. Isis over the Mac's one-way VPN (the Mac may
#: initiate into the VPN by design), which is why this direction works at all.
DEFAULT_FLEET_URL: Final = "http://10.100.0.2:8000"

BEAT_PATH: Final = "/api/devices/heartbeat"

#: Bounded because the caller is unauthenticated: a beat is a few hundred bytes.
MAX_BODY_BYTES: Final = 4096
MAX_DEVICE_LEN: Final = 64
FORWARD_TIMEOUT_S: Final = 8

#: The fleet answers 200; this relay answers 204 (it stores nothing, so it has no
#: body to return). Both are "the beat landed", and insisting on 200 would have made
#: every relayed beat read as a failure to the phone that sent it.
_OK_MIN: Final = 200
_OK_MAX: Final = 300

#: What a phone is allowed to say. `at` is absent ON PURPOSE — the fleet stamps it
#: from its own clock so a beat cannot backdate itself, and `viaLan` is absent
#: because it is this relay's testimony, not the phone's.
_FROM_PHONE: Final = (
    "device",
    "app",
    "version",
    "startedAt",
    "streaming",
    "charging",
    "micOk",
)


class RelayRejected(Exception):
    """The body was not a beat this relay will pass on."""


def relayed(raw: bytes) -> dict[str, Any]:
    """The beat to forward: what the phone said, filtered, plus how it arrived.

    Raises `RelayRejected` rather than forwarding anything questionable — this is
    the boundary between an unauthenticated LAN caller and the fleet's store.
    """
    if len(raw) > MAX_BODY_BYTES:
        msg = f"body is {len(raw)} bytes"
        raise RelayRejected(msg)
    try:
        parsed = json.loads(raw)
    except (ValueError, UnicodeDecodeError) as exc:
        msg = f"not JSON: {exc}"
        raise RelayRejected(msg) from exc
    if not isinstance(parsed, dict):
        msg = f"not an object: {type(parsed).__name__}"
        raise RelayRejected(msg)

    device = parsed.get("device")
    if not isinstance(device, str) or not device:
        msg = "no device"
        raise RelayRejected(msg)
    if len(device) > MAX_DEVICE_LEN:
        msg = f"device name is {len(device)} chars"
        raise RelayRejected(msg)

    out: dict[str, Any] = {k: parsed[k] for k in _FROM_PHONE if k in parsed}
    # Stamped, never copied: see the module docstring.
    out["viaLan"] = True
    return out


def forward(beat: dict[str, Any], fleet_url: str = DEFAULT_FLEET_URL) -> bool:
    """POST one beat to the fleet. Returns whether it landed.

    Best-effort like every other part of this path: a relay that raised its own
    failures would be the tail wagging the dog. The phone learns by the status we
    return, and its own retry backoff (#886) decides what to do about it.
    """
    request = urllib.request.Request(
        f"{fleet_url}{BEAT_PATH}",
        data=json.dumps(beat).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=FORWARD_TIMEOUT_S) as resp:
            status: int = resp.status
            return _OK_MIN <= status < _OK_MAX
    except (urllib.error.URLError, TimeoutError, OSError) as exc:
        _log.warning("could not forward a beat to %s: %s", fleet_url, exc)
        return False


class _Handler(BaseHTTPRequestHandler):
    fleet_url: str = DEFAULT_FLEET_URL

    # No @override: BaseHTTPRequestHandler declares no `do_POST` — it dispatches on
    # the method name at runtime, so this defines the handler rather than overriding.
    def do_POST(self) -> None:
        if self.path != BEAT_PATH:
            self.send_error(404)
            return
        length = int(self.headers.get("Content-Length") or 0)
        if length > MAX_BODY_BYTES:
            self.send_error(413)
            return
        try:
            beat = relayed(self.rfile.read(length))
        except RelayRejected as exc:
            _log.warning("refused a beat: %s", exc)
            self.send_error(400)
            return
        if not forward(beat, self.fleet_url):
            # 502, not 200: the phone must not read "the Mac took it" as "the fleet
            # knows", or a dead Isis would look like three healthy mics.
            self.send_error(502)
            return
        _log.info("relayed a beat from %s", beat.get("device"))
        self.send_response(204)
        self.end_headers()

    @override
    def log_message(self, format: str, *args: Any) -> None:
        """Quiet by default: the agent's log is for beats, not for one line per
        request from a phone that beats every hour anyway."""


def serve(port: int = DEFAULT_RELAY_PORT, fleet_url: str = DEFAULT_FLEET_URL) -> None:
    """Accept beats on the LAN and pass them to the fleet, forever."""
    handler = type("_BoundHandler", (_Handler,), {"fleet_url": fleet_url})
    server = ThreadingHTTPServer(("0.0.0.0", port), handler)
    _log.info("beat relay listening on :%d, forwarding to %s", port, fleet_url)
    server.serve_forever()
