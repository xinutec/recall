"""Pause/resume the always-on recording, via a single pause file.

Completeness (never drop audio) is the system's #1 requirement, so a pause is
always *bounded*: pausing records a "resume-by" time; once it passes the worker
clears it, so recording can never be left off indefinitely if a session is
forgotten.

The pause file under the data root is the single source of truth. Nothing reaches
across processes to stop another: every recording agent (USB capture, live
transcription, each phone listener) *self-gates* on the file — it parks before
opening the device and tears its pipe down when a pause begins, finalising the
current segment so no audio is lost. So a pause is just "write the file"; resume is
just "clear the file". Agents stay loaded throughout (parked while paused), which
makes the health check trivial: every installed agent should always be loaded.

This file is pure file/time logic plus thin launchctl *reads* (never writes), so it
is fully unit-testable.
"""

from __future__ import annotations

import os
import subprocess
from collections.abc import Callable
from datetime import datetime, timedelta
from pathlib import Path
from typing import Protocol

# All recall agents share this prefix; the USB capture agent has this exact label.
_AGENT_PREFIX = "com.pippijn.recall-"
CAPTURE_LABEL = f"{_AGENT_PREFIX}capture"
# A pause lasts at most this long, then recording auto-resumes — a safety net so a
# forgotten pause can't leave recording off indefinitely. 24 h covers a full day away
# and is back on within a day even if the user forgets to re-enable it.
MAX_PAUSE = timedelta(hours=24)
# How often a parked/recording agent re-checks whether its pause is over.
_PAUSE_POLL_SECONDS = 5.0

# Durable capture-lifecycle event kinds (recorded via recall.store.add_capture_event) —
# the record that tells a deliberate pause-gap apart from silently lost audio, so an
# UNEXPLAINED gap (= unrecoverable lost speech) can be detected instead of hidden.
EVENT_PAUSE = "pause"
EVENT_RESUME = "resume"
EVENT_DEAD_WINDOW = "dead_window"


def _pause_file(root: Path) -> Path:
    return root / "capture_paused_until"


def compute_resume_by(now: datetime, minutes: int | None) -> datetime:
    """When a pause starting at `now` must end — clamped to MAX_PAUSE."""
    span = MAX_PAUSE if minutes is None else min(timedelta(minutes=minutes), MAX_PAUSE)
    return now + max(span, timedelta(0))


def write_pause_until(root: Path, until: datetime) -> None:
    _pause_file(root).write_text(until.isoformat())


def clear_pause(root: Path) -> None:
    _pause_file(root).unlink(missing_ok=True)


def paused_until(root: Path) -> datetime | None:
    """The recorded resume-by time, or None if not paused."""
    f = _pause_file(root)
    if not f.exists():
        return None
    try:
        return datetime.fromisoformat(f.read_text().strip())
    except ValueError:
        return None


def is_paused(root: Path, now: datetime) -> bool:
    until = paused_until(root)
    return until is not None and until > now


def wait_until_unpaused(
    root: Path,
    *,
    now: Callable[[], datetime],
    sleep: Callable[[float], None],
    poll_seconds: float = _PAUSE_POLL_SECONDS,
) -> None:
    """Block while a pause is active, returning once it's cleared or has elapsed.

    Recording agents call this before opening their device, so a fresh start (login,
    KeepAlive respawn, manual launch) never records straight through an active pause.
    Parking (rather than exiting) keeps the agent alive with no respawn loop; it
    simply records nothing until the pause is legitimately over. Returns immediately
    when not paused.
    """
    while is_paused(root, now()):
        sleep(poll_seconds)


def pause(root: Path, now: datetime, *, minutes: int | None = None) -> datetime:
    """Begin a (bounded) pause: write the resume-by time. The recording agents see
    the file and stop themselves; the phones' reachability gate stops them too once
    their listener releases its port. Returns when it will auto-resume by."""
    until = compute_resume_by(now, minutes)
    write_pause_until(root, until)
    return until


def resume(root: Path) -> None:
    """End the pause: clear the file. Parked recording agents resume on their own."""
    clear_pause(root)


def auto_resume_if_expired(root: Path, now: datetime) -> bool:
    """Worker safety net: clear an elapsed pause so recording resumes. True if so."""
    until = paused_until(root)
    if until is not None and until <= now:
        resume(root)
        return True
    return False


# --- Fleet-side capture intent (the Isis split) ---
#
# The fleet (Isis) is where the web UI is reachable over the VPN, but it runs no capture
# agent — a pause file there actuates nothing. So the fleet holds only the *desired*
# state (this "intent"); the Mac polls it (see recall.capture_mirror) and mirrors it
# onto its own pause file, where the capture agents self-gate. Isis is the authority:
# the button lives where the UI is, the mic obeys where it physically is. The Mac can't
# be dialled (one-way WireGuard peer), so control inverts to a Mac-initiated poll.

FLEET_ROLE = "fleet"
_INTENT_KEY = "capture_intent"  # ISO resume-by; blank/absent = running
# The Mac reports what it actually applied so the fleet's status shows reality, not just
# intent — a pause you cannot confirm took effect is worthless for a privacy control.
_REPORTED_RUNNING_KEY = "capture_reported_running"
_REPORTED_PAUSED_KEY = "capture_reported_paused_until"
_REPORTED_AT_KEY = "capture_reported_at"
# A report older than this means the Mac has stopped checking in; the fleet then falls
# back to showing intent (and the separate fleetwatch alarm covers a dead Mac).
_REPORT_FRESH = timedelta(seconds=30)


class _Settings(Protocol):
    """The slice of Store the intent needs — kept structural so capture_control stays
    free of a Store import (and the logic is testable with a trivial fake)."""

    def get_setting(self, key: str) -> str | None: ...
    def set_setting(self, key: str, value: str) -> None: ...


def is_fleet() -> bool:
    """True on the system-of-record node (Isis), which holds capture intent but runs no
    capture agent. Read from an explicit env, not inferred: the Mac also sets
    RECALL_SYNC_TOKEN, so the token can't tell the two roles apart."""
    return os.environ.get("RECALL_ROLE") == FLEET_ROLE


def intent_pause(
    store: _Settings, now: datetime, *, minutes: int | None = None
) -> datetime:
    """Record a (bounded) pause as the fleet's desired state. Returns its resume-by."""
    until = compute_resume_by(now, minutes)
    store.set_setting(_INTENT_KEY, until.isoformat())
    return until


def intent_resume(store: _Settings) -> None:
    """Record "run" as the fleet's desired state (clear the pause intent)."""
    store.set_setting(_INTENT_KEY, "")


def intent_until(store: _Settings, now: datetime) -> datetime | None:
    """The desired resume-by, or None when running. An elapsed intent reads as running —
    the same bounded-pause safety net the local file has via auto_resume_if_expired."""
    raw = store.get_setting(_INTENT_KEY)
    if not raw:
        return None
    try:
        until = datetime.fromisoformat(raw)
    except ValueError:
        return None
    return until if until > now else None


def record_reported(
    store: _Settings, *, running: bool, paused_until: str | None, now: datetime
) -> None:
    """Store the Mac's just-applied capture state, so the fleet's status is honest."""
    store.set_setting(_REPORTED_RUNNING_KEY, "1" if running else "0")
    store.set_setting(_REPORTED_PAUSED_KEY, paused_until or "")
    store.set_setting(_REPORTED_AT_KEY, now.isoformat())


def reported_state(store: _Settings, now: datetime) -> tuple[bool, str | None] | None:
    """The Mac's last-reported (running, pausedUntil) if fresh, else None — the Mac has
    stopped reporting, so the caller shows intent instead."""
    at = store.get_setting(_REPORTED_AT_KEY)
    running = store.get_setting(_REPORTED_RUNNING_KEY)
    if not at or running is None:
        return None
    try:
        at_dt = datetime.fromisoformat(at)
    except ValueError:
        return None
    if now - at_dt > _REPORT_FRESH:
        return None
    return running == "1", store.get_setting(_REPORTED_PAUSED_KEY)


# --- launchctl reads (thin; for status + the health check) ---


def loaded_agents() -> set[str]:
    """Recall agent labels currently loaded in launchd. Empty when launchctl isn't
    there — e.g. the fleet's Linux container serving the api (the Isis split): capture
    runs on the Mac, so "no agents loaded" is the right answer, not a crash."""
    try:
        out = subprocess.run(
            ["launchctl", "list"], capture_output=True, text=True, check=False
        )
    except FileNotFoundError:
        return set()
    return {
        parts[-1]
        for line in out.stdout.splitlines()
        if (parts := line.split()) and parts[-1].startswith(_AGENT_PREFIX)
    }


def installed_agents() -> list[str]:
    """Labels of every installed recall agent (its plist in ~/Library/LaunchAgents)."""
    agents = Path.home() / "Library" / "LaunchAgents"
    return sorted(p.stem for p in agents.glob(f"{_AGENT_PREFIX}*.plist"))


def agent_health() -> list[tuple[str, bool]]:
    """(label, loaded?) for every installed agent. Self-gating means agents stay
    loaded even while paused, so any installed-but-not-loaded agent is a fault."""
    loaded = loaded_agents()
    return [(label, label in loaded) for label in installed_agents()]


def capture_running() -> bool:
    """Whether the USB capture agent is loaded (parked or actively recording)."""
    return CAPTURE_LABEL in loaded_agents()
