"""Mac-side capture mirror: it makes the local pause file match the fleet's intent, but
only when that intent *changes* — so a pause set on the Mac's own UI is not stamped out
every cycle by an unchanged "running" intent from the fleet."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from pathlib import Path

from recall import capture_control, capture_mirror

NOW = datetime(2026, 7, 14, 12, 0, 0, tzinfo=UTC)
FUTURE = (NOW + timedelta(hours=1)).isoformat()


class FakeExchange:
    """Returns a fixed fleet intent and records what the Mac reported each pass."""

    def __init__(self, intent: str | None) -> None:
        self.intent = intent
        self.reported: list[tuple[bool, str | None]] = []

    def exchange_capture(
        self, *, running: bool, paused_until: str | None
    ) -> str | None:
        self.reported.append((running, paused_until))
        return self.intent


def test_a_fleet_pause_is_applied_to_the_local_mic(tmp_path: Path) -> None:
    client = FakeExchange(FUTURE)
    assert capture_mirror.reconcile_once(tmp_path, client, now=NOW) is True
    assert capture_control.is_paused(tmp_path, NOW)
    assert capture_control.paused_until(tmp_path) == datetime.fromisoformat(FUTURE)


def test_a_fleet_resume_clears_a_mirrored_pause(tmp_path: Path) -> None:
    paused = FakeExchange(FUTURE)
    capture_mirror.reconcile_once(tmp_path, paused, now=NOW)
    assert capture_control.is_paused(tmp_path, NOW)

    running = FakeExchange(None)
    assert capture_mirror.reconcile_once(tmp_path, running, now=NOW) is True
    assert not capture_control.is_paused(tmp_path, NOW)


def test_an_unchanged_intent_is_a_no_op(tmp_path: Path) -> None:
    client = FakeExchange(FUTURE)
    assert capture_mirror.reconcile_once(tmp_path, client, now=NOW) is True
    assert capture_mirror.reconcile_once(tmp_path, client, now=NOW) is False


def test_an_unchanged_running_intent_does_not_clobber_a_local_pause(
    tmp_path: Path,
) -> None:
    # The reason for edge-triggering: the fleet says "running" and never changes, while
    # the household pauses the mic from the Mac's own LAN UI. The mirror must leave that
    # local pause alone, not resume the mic under them every 5 seconds.
    running = FakeExchange(None)
    capture_mirror.reconcile_once(tmp_path, running, now=NOW)  # marker settles: running

    capture_control.pause(tmp_path, NOW, minutes=15)  # a local pause via the Mac UI
    assert capture_control.is_paused(tmp_path, NOW)

    assert capture_mirror.reconcile_once(tmp_path, running, now=NOW) is False
    assert capture_control.is_paused(tmp_path, NOW)  # survived


def test_each_pass_reports_the_macs_current_state(tmp_path: Path) -> None:
    capture_control.pause(tmp_path, NOW, minutes=15)
    client = FakeExchange(None)
    capture_mirror.reconcile_once(tmp_path, client, now=NOW)
    running, paused_until = client.reported[0]
    assert running is False
    local = capture_control.paused_until(tmp_path)
    assert local is not None
    assert paused_until == local.isoformat()


def test_a_fleet_pause_already_elapsed_is_treated_as_running(tmp_path: Path) -> None:
    # A stale intent whose resume-by has passed must not re-pause the mic.
    past = (NOW - timedelta(minutes=1)).isoformat()
    client = FakeExchange(past)
    capture_mirror.reconcile_once(tmp_path, client, now=NOW)
    assert not capture_control.is_paused(tmp_path, NOW)
