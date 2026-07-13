"""Report recall's health to fleetwatch — the fleet's monitoring platform.

fleetwatch is push-based: a producer POSTs verdict-shaped reports and it keeps their
history (see its README, "The report contract"). Two properties make it the right place
for this, and they are why nothing here needs to send mail or run a daemon that watches
a daemon:

* **A dead producer is a failure, not a silence.** A report declares its own cadence
  (`interval_s`); fleetwatch renders a producer that has stopped reporting as `Silent` —
  red. So this need not detect the case where the *Mac itself* dies: not reporting IS
  the report. A monitor that only speaks when it is well is no monitor at all.
* The verdicts already show up where the rest of the fleet's health does, which is where
  a fault will actually be seen.

The ingest token is read from `~/.config/fleetwatch/token` and never leaves this machine
— it is not in the repo, not in the report, and not logged.
"""

from __future__ import annotations

import json
import os
import secrets
import urllib.error
import urllib.request
from collections.abc import Sequence
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path

from recall.health import Check

DEFAULT_URL = "https://fleetwatch.xinutec.org/api/reports"
TOKEN_FILE = Path.home() / ".config" / "fleetwatch" / "token"
# One producer for the whole of recall: is it recording, are its agents up, is the
# archive mirrored. Kept as one collector so a single fleetwatch tile answers "is
# recall alright?" — the question actually being asked.
COLLECTOR = "recall"
# Declared cadence. fleetwatch turns this into staleness: report less often than
# this and the tile goes amber, stop entirely and it goes red. It must match the
# launchd agent's StartInterval, or a healthy producer is reported as late.
INTERVAL_S = 300
_SCHEMA = 1
_CROCKFORD = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"  # no I, L, O, U — a ULID alphabet


def mint_ulid(now: datetime, randomness: bytes) -> str:
    """A ULID: 48 bits of millisecond timestamp, then 80 bits of randomness, in
    Crockford base32. fleetwatch uses it as the idempotency key and rejects anything
    that is not one (422), so it is minted here rather than depending on a library for
    twelve lines. Pure, so it is tested against a known vector.
    """
    if len(randomness) != 10:  # noqa: PLR2004 - the ULID spec's 80 bits
        raise ValueError("a ULID needs exactly 80 bits of randomness")
    value = (int(now.timestamp() * 1000) << 80) | int.from_bytes(randomness, "big")
    return "".join(_CROCKFORD[(value >> shift) & 0x1F] for shift in range(125, -1, -5))


@dataclass(frozen=True)
class Report:
    """One fleetwatch report. `source` is deliberately absent: fleetwatch stamps it from
    the ingest token, so a producer can only ever write as itself."""

    id: str
    collected_at: datetime
    checks: tuple[Check, ...]
    duration_ms: int | None = None

    def payload(self) -> dict[str, object]:
        return {
            "schema": _SCHEMA,
            "id": self.id,
            "collector": COLLECTOR,
            "collected_at": self.collected_at.isoformat(),
            "duration_ms": self.duration_ms,
            "interval_s": INTERVAL_S,
            "checks": [
                {
                    "section": check.section,
                    "label": check.label,
                    "verdict": check.verdict,
                    "observed": check.observed,
                    "expected": check.expected,
                    "value": check.value,
                    "unit": check.unit,
                }
                for check in self.checks
            ],
        }


def build_report(
    checks: Sequence[Check],
    *,
    now: datetime,
    randomness: bytes | None = None,
    duration_ms: int | None = None,
) -> Report:
    return Report(
        id=mint_ulid(
            now, randomness if randomness is not None else secrets.token_bytes(10)
        ),
        collected_at=now,
        checks=tuple(checks),
        duration_ms=duration_ms,
    )


def read_token(path: Path = TOKEN_FILE) -> str | None:
    """The ingest token, from the environment or the file the fleet's secret.sh writes.

    None if there is none — this machine is simply not a producer yet, which is a thing
    to say plainly, not to crash on.
    """
    from_env = os.environ.get("RECALL_FLEETWATCH_TOKEN")
    if from_env:
        return from_env.strip()
    try:
        token = path.read_text().strip()
    except OSError:
        return None
    return token or None


def post_report(report: Report, *, token: str, url: str = DEFAULT_URL) -> int:
    """POST one report. Returns the HTTP status (201 stored, 200 duplicate).

    Raises on a transport failure — the caller decides whether a fleetwatch it cannot
    reach is worth failing over. It is not: the recording is what matters, and an
    unreachable monitor already shows itself as stale at the other end.
    """
    request = urllib.request.Request(
        url,
        data=json.dumps(report.payload()).encode(),
        headers={
            "Content-Type": "application/json",
            "Authorization": f"Bearer {token}",
        },
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=15) as response:
        return int(response.status)
