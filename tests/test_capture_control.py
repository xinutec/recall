"""Pause-state logic for pausing/resuming capture (the launchctl calls are thin)."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from pathlib import Path

from recall import capture_control as cc

NOW = datetime(2026, 6, 17, 12, 0, 0, tzinfo=UTC)


def test_not_paused_when_no_file(tmp_path: Path) -> None:
    assert cc.paused_until(tmp_path) is None
    assert cc.is_paused(tmp_path, NOW) is False


def test_write_and_read_pause(tmp_path: Path) -> None:
    until = NOW + timedelta(minutes=20)
    cc.write_pause_until(tmp_path, until)
    assert cc.paused_until(tmp_path) == until
    assert cc.is_paused(tmp_path, NOW) is True
    # ...but once the time passes, it's no longer an active pause.
    assert cc.is_paused(tmp_path, until + timedelta(seconds=1)) is False


def test_resume_by_is_clamped_to_max(tmp_path: Path) -> None:
    # A huge request is capped; None means the default cap.
    # A forgotten pause is back on within a day.
    assert timedelta(hours=24) == cc.MAX_PAUSE
    assert cc.compute_resume_by(NOW, 99999) == NOW + cc.MAX_PAUSE
    assert cc.compute_resume_by(NOW, None) == NOW + cc.MAX_PAUSE
    assert cc.compute_resume_by(NOW, 10) == NOW + timedelta(minutes=10)


def test_clear_pause(tmp_path: Path) -> None:
    cc.write_pause_until(tmp_path, NOW + timedelta(minutes=5))
    cc.clear_pause(tmp_path)
    assert cc.paused_until(tmp_path) is None
    cc.clear_pause(tmp_path)  # idempotent — no error when already clear


def test_pause_writes_the_file_and_resume_clears_it(tmp_path: Path) -> None:
    # A pause is just the file now (no reaching across to stop agents); agents
    # self-gate on it. Resume just clears it.
    until = cc.pause(tmp_path, NOW, minutes=20)
    assert until == NOW + timedelta(minutes=20)
    assert cc.is_paused(tmp_path, NOW) is True
    cc.resume(tmp_path)
    assert cc.is_paused(tmp_path, NOW) is False


def test_agent_health_flags_installed_but_unloaded(monkeypatch: object) -> None:
    # Self-gating means every installed agent should always be loaded; one that is
    # installed but not loaded is the fault the health check exists to catch.
    monkeypatch.setattr(  # type: ignore[attr-defined]
        cc,
        "installed_agents",
        lambda: ["com.pippijn.recall-api", "com.pippijn.recall-capture"],
    )
    monkeypatch.setattr(  # type: ignore[attr-defined]
        cc, "loaded_agents", lambda: {"com.pippijn.recall-api"}
    )
    assert cc.agent_health() == [
        ("com.pippijn.recall-api", True),
        ("com.pippijn.recall-capture", False),  # installed but not loaded → fault
    ]


def test_wait_until_unpaused_returns_immediately_when_not_paused(
    tmp_path: Path,
) -> None:
    slept: list[float] = []
    cc.wait_until_unpaused(
        tmp_path, now=lambda: NOW, sleep=slept.append, poll_seconds=5.0
    )
    assert slept == []  # nothing to wait out


def test_wait_until_unpaused_blocks_until_pause_elapses(tmp_path: Path) -> None:
    # The clock advances one poll-interval per sleep, so the pause expires after
    # a few polls; the loop must keep sleeping until then, not record meanwhile.
    cc.write_pause_until(tmp_path, NOW + timedelta(seconds=12))
    clock = [NOW]
    slept: list[float] = []

    def sleep(seconds: float) -> None:
        slept.append(seconds)
        clock[0] = clock[0] + timedelta(seconds=seconds)

    cc.wait_until_unpaused(
        tmp_path, now=lambda: clock[0], sleep=sleep, poll_seconds=5.0
    )
    assert slept == [5.0, 5.0, 5.0]  # waited out the 12s pause, then returned


def test_wait_until_unpaused_returns_when_pause_cleared_mid_wait(
    tmp_path: Path,
) -> None:
    # Resume (clearing the pause file) while blocked must release the wait.
    cc.write_pause_until(tmp_path, NOW + timedelta(minutes=30))
    slept: list[float] = []

    def sleep(seconds: float) -> None:
        slept.append(seconds)
        cc.clear_pause(tmp_path)  # someone pressed Resume

    cc.wait_until_unpaused(tmp_path, now=lambda: NOW, sleep=sleep, poll_seconds=5.0)
    assert slept == [5.0]  # one poll, then the cleared pause released it


def test_auto_resume_only_fires_after_expiry(tmp_path: Path) -> None:
    cc.write_pause_until(tmp_path, NOW + timedelta(minutes=30))
    assert cc.auto_resume_if_expired(tmp_path, NOW) is False  # still paused
    assert cc.is_paused(tmp_path, NOW) is True

    assert cc.auto_resume_if_expired(tmp_path, NOW + timedelta(minutes=31)) is True
    assert cc.paused_until(tmp_path) is None  # pause cleared → parked agents resume
