"""Delivered-segment liveness: the second way a recorder proves it is recording.

A STORE-AND-FORWARD recorder streams to nothing, so the `.alive` markers
`/api/sources` was built on are never refreshed for it — it reads dead while
recording perfectly (#1428). geb was the first to hit this: its marker froze the
minute it stopped streaming and its dot went out while it kept delivering.

recalld holds the evidence that does exist — the newest delivered segment per
source — and serves it on `/ingest/v1/liveness`. In the pod both containers
already carry the same secret (the api as `RECALL_SYNC_TOKEN`, recalld as
`RECALLD_READ_TOKEN`), so this needs no new credential.

⚠ The times are segment CAPTURE times, never arrival times. A cached backlog
draining hours late arrives now and would read as "recording now" while proving
nothing about now — recalld picks the right column; this module must not
substitute a local clock for it.

Best-effort by construction: an unreachable or slow recalld means "no extra
evidence", never an error page. The panel degrades to exactly the marker view it
had before, which is why every failure here returns an empty mapping.
"""

from __future__ import annotations

import logging
import os
from datetime import UTC, datetime

import httpx

_log = logging.getLogger("recall.ingest_liveness")

URL_ENV = "RECALLD_URL"
TOKEN_ENV = "RECALL_SYNC_TOKEN"
# The sidecar shares the pod's loopback, so the default needs no configuration
# on the fleet. The Mac sets RECALLD_URL to reach Isis (it may dial out; Isis
# cannot dial in).
DEFAULT_URL = "http://127.0.0.1:8001"
# Short: this sits inside a UI poll. A recalld that cannot answer promptly is
# indistinguishable from one with nothing to add, and both mean "show markers".
TIMEOUT_S = 1.5


def _parse(stamp: str) -> datetime | None:
    """recalld writes `%Y-%m-%dT%H:%M:%SZ` (audiocore::names). Anything else is a
    contract change, not a value to guess at."""
    try:
        return datetime.strptime(stamp, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=UTC)
    except ValueError:
        _log.warning("unparsable capture stamp from recalld: %r", stamp)
        return None


def delivered_liveness(*, client: httpx.Client | None = None) -> dict[str, datetime]:
    """Each source's newest delivered-segment CAPTURE time, or {} if unavailable."""
    token = os.environ.get(TOKEN_ENV)
    if not token:
        return {}
    url = os.environ.get(URL_ENV, DEFAULT_URL).rstrip("/")
    try:
        request = client or httpx.Client(timeout=TIMEOUT_S)
        try:
            response = request.get(
                f"{url}/ingest/v1/liveness",
                headers={"Authorization": f"Bearer {token}"},
            )
            response.raise_for_status()
            payload = response.json()
        finally:
            if client is None:
                request.close()
    except (httpx.HTTPError, ValueError) as err:
        # Not an error path for the caller: the panel still has its markers.
        _log.debug("recalld liveness unavailable: %s", err)
        return {}
    sources = payload.get("sources") or {}
    if not isinstance(sources, dict):
        _log.warning("recalld liveness payload is not a mapping: %r", type(sources))
        return {}
    out: dict[str, datetime] = {}
    for source, stamp in sources.items():
        when = _parse(stamp) if isinstance(stamp, str) else None
        if when is not None:
            out[str(source)] = when
    return out
