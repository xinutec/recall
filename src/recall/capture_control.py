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

import subprocess
from collections.abc import Callable
from datetime import datetime, timedelta
from pathlib import Path

# All recall agents share this prefix; the USB capture agent has this exact label.
_AGENT_PREFIX = "com.pippijn.recall-"
CAPTURE_LABEL = f"{_AGENT_PREFIX}capture"
# A pause lasts at most this long, then recording auto-resumes — a safety net so a
# forgotten pause can't leave recording off indefinitely. 24 h covers a full day away
# and is back on within a day even if the user forgets to re-enable it.
MAX_PAUSE = timedelta(hours=24)
# How often a parked/recording agent re-checks whether its pause is over.
_PAUSE_POLL_SECONDS = 5.0


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
