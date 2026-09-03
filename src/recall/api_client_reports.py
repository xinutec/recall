"""What the browser tells the server: error reports and the activity trace.

Slice 3 of api.py's decomposition (#1342). The client log path is injected;
`one_line` is public here because it is the telemetry endpoint's security
boundary and is tested directly.
"""

from __future__ import annotations

import logging
import unicodedata
from datetime import UTC, datetime
from pathlib import Path

from fastapi import FastAPI

from recall.api_models import ClientLog, TelemetryEvent
from recall.schemas import OkOut

_log = logging.getLogger("recall.api")

# A per-batch cap so a buggy client cannot turn one POST into a log flood, and a
# label cap so a pathological one cannot bloat a line. Counted in code points,
# not bytes, so a multi-byte glyph is never split.
_MAX_EVENTS = 100
_MAX_LABEL = 160


def _one_line(label: str, max_len: int) -> str:
    """Flatten a client-supplied label to a single harmless log field.

    The security boundary of the telemetry endpoint, not tidiness. A label is
    verbatim UI text written into a log line as ``label=…``, so a newline inside
    it forges *whole log lines* — including further ``client-event`` lines
    attributed to someone else. The log stops being evidence, which is the one
    thing it exists to be.

    ``str.split()`` with no argument splits on every Unicode whitespace,
    including the U+2028/U+2029 separators that are not control characters; the
    category pass ahead of it catches the format and control characters that are
    not whitespace at all.
    """
    unbroken = "".join(
        " " if unicodedata.category(c) in {"Cc", "Cf", "Zl", "Zp"} else c for c in label
    )
    return " ".join(unbroken.split())[:max_len]


def register_client_report_routes(app: FastAPI, *, client_log_path: Path) -> None:
    """Mount /api/log + /api/telemetry."""

    @app.post("/api/log")
    def client_log(body: ClientLog) -> OkOut:
        """Record a browser-side error/event to logs/client.log (the phone has
        no console you can read)."""
        stamp = datetime.now(UTC).isoformat(timespec="seconds")
        parts = [stamp, f"[{body.level}]", body.url or "-", body.message]
        if body.stack:
            parts.append(f"\n    {body.stack.splitlines()[0]}")
        client_log_path.parent.mkdir(parents=True, exist_ok=True)
        with client_log_path.open("a") as fh:
            fh.write(" ".join(parts) + "\n")
        return {"ok": True}

    @app.post("/api/telemetry")
    def telemetry(events: list[TelemetryEvent]) -> OkOut:
        """Record what the person did, beside what the API was asked for.

        Distinct from ``/api/log`` above, which reports browser *errors* to a file.
        This is the activity trace: a tap that hits a cache, a control that was
        disabled, a screen that rendered wrong — none of it reaches the server
        otherwise, so "I pressed it and nothing happened" is undiagnosable.

        Goes to the application logger rather than ``logs/client.log`` deliberately,
        so it interleaves with the request log and a session reads as one timeline.
        There is no storage: these are logs, not data.

        Same ``client-event`` line shape as every other app in the fleet — the whole
        value is being able to grep one word anywhere and get the same fields.
        """
        for e in events[:_MAX_EVENTS]:
            label = _one_line(e.label or "", _MAX_LABEL)
            _log.info(
                "client-event kind=%s path=%s label=%s at=%s",
                e.kind,
                e.path,
                label,
                e.at,
            )
        return {"ok": True}
