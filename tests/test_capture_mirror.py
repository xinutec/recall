"""Mac-side capture mirror: it makes the local pause file match the fleet's intent, but
only when that intent *changes* — so a pause set on the Mac's own UI is not stamped out
every cycle by an unchanged "running" intent from the fleet."""

from __future__ import annotations

import os
from collections.abc import Mapping
from datetime import UTC, datetime, timedelta
from pathlib import Path

from recall import capture_control, capture_mirror
from recall.stream_server import ALIVE_FILE

NOW = datetime(2026, 7, 14, 12, 0, 0, tzinfo=UTC)
FUTURE = (NOW + timedelta(hours=1)).isoformat()


class FakeExchange:
    """Returns a fixed fleet intent and records what the Mac reported each pass."""

    def __init__(self, intent: str | None) -> None:
        self.intent = intent
        self.reported: list[tuple[bool, str | None]] = []
        self.liveness: list[dict[str, str]] = []

    def exchange_capture(
        self,
        *,
        running: bool,
        paused_until: str | None,
        source_liveness: Mapping[str, str],
    ) -> str | None:
        self.reported.append((running, paused_until))
        self.liveness.append(dict(source_liveness))
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


def test_a_malformed_intent_reads_as_running_and_does_not_wedge(tmp_path: Path) -> None:
    # A garbage resume-by from the fleet must not throw: an unparseable intent is
    # treated as "running" (the same conservative fallback paused_until/intent_until
    # use). Before this guard, the raise skipped the marker write, so the mirror
    # retried the same poisoned value every tick forever — and a real pause pressed
    # later on the UI would never have been applied.
    capture_control.pause(tmp_path, NOW, minutes=15)  # a pre-existing local pause
    client = FakeExchange("not-a-timestamp")
    assert capture_mirror.reconcile_once(tmp_path, client, now=NOW) is True
    assert not capture_control.is_paused(tmp_path, NOW)  # fallback: running
    # The marker recorded it, so the next identical poll is a no-op, not a retry loop.
    assert capture_mirror.reconcile_once(tmp_path, client, now=NOW) is False


def test_reconcile_reports_an_applied_intent_to_the_hook(tmp_path: Path) -> None:
    # The hook is the durable "intent-seen" timestamp of the resume timeline
    # (docs/capture-loss-plan.md Phase 1): called only when intent actually changes,
    # with what was applied — a pause's resume-by, or "" for running.
    applied: list[str] = []
    paused = FakeExchange(FUTURE)
    capture_mirror.reconcile_once(tmp_path, paused, now=NOW, on_applied=applied.append)
    assert applied == [FUTURE]

    capture_mirror.reconcile_once(tmp_path, paused, now=NOW, on_applied=applied.append)
    assert applied == [FUTURE]  # unchanged intent -> not called

    running = FakeExchange(None)
    capture_mirror.reconcile_once(tmp_path, running, now=NOW, on_applied=applied.append)
    assert applied == [FUTURE, ""]


def test_a_failing_hook_never_blocks_the_intent_application(tmp_path: Path) -> None:
    # Telemetry must not be able to wedge the mirror — the pause still applies.
    def boom(_intent: str) -> None:
        raise RuntimeError("db locked")

    client = FakeExchange(FUTURE)
    assert (
        capture_mirror.reconcile_once(tmp_path, client, now=NOW, on_applied=boom)
        is True
    )
    assert capture_control.is_paused(tmp_path, NOW)


def test_each_pass_ships_the_phones_alive_freshness(tmp_path: Path) -> None:
    # The fleet can't see the phones' .alive markers (they're on the Mac), so the mirror
    # reports each one's last-active time — that's what makes /api/sources truthful on
    # Isis. The USB mic has no marker and must not appear here.
    marker_time = datetime(2026, 7, 14, 11, 59, 30, tzinfo=UTC)
    phone = tmp_path / "pixel9"
    phone.mkdir()
    (phone / ALIVE_FILE).touch()
    os.utime(phone / ALIVE_FILE, (marker_time.timestamp(), marker_time.timestamp()))

    client = FakeExchange(None)
    capture_mirror.reconcile_once(tmp_path, client, now=NOW)
    assert client.liveness[0] == {"pixel9": marker_time.isoformat()}


def test_liveness_report_is_empty_with_no_phones(tmp_path: Path) -> None:
    client = FakeExchange(None)
    capture_mirror.reconcile_once(tmp_path, client, now=NOW)
    assert client.liveness[0] == {}
