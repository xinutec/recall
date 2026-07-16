"""Mac-side capture-intent mirror — the mic half of the Isis split.

Isis holds the *desired* capture state (pause/resume from its VPN-reachable UI) but runs
no capture agent, so it cannot stop the mic itself. The Mac is a one-way WireGuard peer
— Isis can't dial it — so the Mac POLLS Isis's intent and mirrors it onto its local
pause file, which the capture agents already self-gate on. Each pass reports what the
Mac applied, so Isis's status shows reality rather than just intent.

Short-poll every ~5s, matching the capture agents' own 5s self-gate: the transport is
never the bottleneck, and each poll is independent — a dropped packet or an Isis pod
rollout just means the next tick succeeds, with no held connection to reconnect. (If the
self-gate is ever tightened below 5s and instant delivery matters, an SSE stream would
be the upgrade; not worth its held-connection machinery today.)

Edge-triggered: Isis intent is applied only when it *changes* (tracked in a local marker
file), so a pause set on the Mac's own LAN UI is not clobbered every cycle by an
unchanged "running" intent.

Pure file + injected client, so the reconcile logic is unit-tested with a fake exchange.
"""

from __future__ import annotations

import logging
from collections.abc import Callable, Mapping
from datetime import UTC, datetime
from pathlib import Path
from typing import Protocol

from recall import capture_control
from recall.stream_server import ALIVE_FILE

_log = logging.getLogger("recall.capture_mirror")

# Records the last Isis intent value we applied, so a pass is a no-op when intent is
# unchanged. A plain file (like the pause file itself), so the mirror needs no DB.
_MARKER = "capture_intent_mirrored"


class IntentExchange(Protocol):
    """What the mirror needs of a client — `SyncClient` satisfies it structurally."""

    def exchange_capture(
        self,
        *,
        running: bool,
        paused_until: str | None,
        source_liveness: Mapping[str, str],
    ) -> str | None: ...


def _source_liveness(root: Path) -> dict[str, str]:
    """Each remote recorder's last-active ISO time, read from the .alive markers the
    ingest server refreshes while a phone streams (recall.stream_server). The USB mic
    has no marker — its liveness is the global running state — so only phones appear
    here. The fleet can't see these files (they're on the Mac), so the mirror ships them
    each pass. Store-free, matching the mirror's file-only design; an unreadable marker
    is skipped."""
    out: dict[str, str] = {}
    for marker in root.glob(f"*/{ALIVE_FILE}"):
        try:
            mtime = marker.stat().st_mtime
        except OSError:
            continue
        out[marker.parent.name] = datetime.fromtimestamp(mtime, tz=UTC).isoformat()
    return out


def _marker_file(root: Path) -> Path:
    return root / _MARKER


def _read_marker(root: Path) -> str:
    f = _marker_file(root)
    return f.read_text().strip() if f.exists() else ""


def _write_marker(root: Path, value: str) -> None:
    _marker_file(root).write_text(value)


def _apply(root: Path, intent: str | None, now: datetime) -> None:
    """Make the local pause file match the fleet's intent: a future resume-by pauses,
    anything else (running, or an already-elapsed pause) resumes."""
    if intent:
        until = datetime.fromisoformat(intent)
        if until > now:
            capture_control.write_pause_until(root, until)
            return
    capture_control.clear_pause(root)


def reconcile_once(
    root: Path,
    client: IntentExchange,
    *,
    now: datetime,
    on_applied: Callable[[str], None] | None = None,
) -> bool:
    """One mirror pass: report the Mac's local capture state, pull the fleet's intent,
    and apply it iff it changed since we last did. Returns whether the pause file moved.

    `on_applied` (optional) is called with the just-applied intent ("" = running) —
    the durable "intent-seen" timestamp a resume timeline starts from (the caller
    records it as a capture event). Best-effort: a failing hook never blocks the
    application, which has already happened.
    """
    local_until = capture_control.paused_until(root)
    running = not capture_control.is_paused(root, now)
    intent = client.exchange_capture(
        running=running,
        paused_until=local_until.isoformat() if local_until else None,
        source_liveness=_source_liveness(root),
    )
    intent_value = intent or ""
    if intent_value == _read_marker(root):
        return False  # fleet intent unchanged since we last applied it — leave local be
    _apply(root, intent, now)
    _write_marker(root, intent_value)
    _log.info("fleet intent changed — applied: %s", intent_value or "running")
    if on_applied is not None:
        try:
            on_applied(intent_value)
        except Exception:
            _log.exception("on_applied hook failed (the intent was still applied)")
    return True


def run_loop(  # noqa: PLR0913 - injected clock/sleep + the telemetry hook
    root: Path,
    client: IntentExchange,
    *,
    now: Callable[[], datetime],
    sleep: Callable[[float], None],
    interval: float = 5.0,
    on_applied: Callable[[str], None] | None = None,
) -> None:
    """Mirror the fleet's capture intent forever. A transient network error is logged
    and the loop continues — a blip must never wedge the mic; the next tick retries."""
    while True:
        try:
            reconcile_once(root, client, now=now(), on_applied=on_applied)
        except Exception:  # a poll failure must not kill the mirror
            _log.exception("capture-mirror pass failed; will retry next tick")
        sleep(interval)
