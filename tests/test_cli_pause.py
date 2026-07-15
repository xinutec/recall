"""Local pause/resume CLI — the network-free break-glass control.

Isis is the normal pause/resume surface (its VPN UI → capture-mirror), but the Mac has
no local control once its own UI is retired. When Isis is unreachable (a pod rollout, a
network blip) that leaves no way to change capture state at all. `recall pause` /
`recall resume` write the same pause file the capture agents self-gate on, directly and
with no network, so control always exists on the machine that holds the mic.
"""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from pathlib import Path

from recall import capture_control, cli
from recall.cli_parser import build_parser


def test_pause_writes_a_bounded_pause_file(tmp_path: Path) -> None:
    rc = cli.main(["pause", "--out", str(tmp_path), "--minutes", "30"])
    assert rc == 0
    until = capture_control.paused_until(tmp_path)
    assert until is not None
    now = datetime.now(UTC)
    # ~30 min out (generous window for wall-clock between the two now() reads).
    assert now + timedelta(minutes=29) < until < now + timedelta(minutes=31)
    assert capture_control.is_paused(tmp_path, now)


def test_pause_without_minutes_uses_the_max_bounded_pause(tmp_path: Path) -> None:
    # No duration = the safety-net maximum (never an unbounded pause: recording must
    # always come back on its own if a pause is forgotten).
    rc = cli.main(["pause", "--out", str(tmp_path)])
    assert rc == 0
    until = capture_control.paused_until(tmp_path)
    assert until is not None
    now = datetime.now(UTC)
    assert until > now + capture_control.MAX_PAUSE - timedelta(minutes=1)
    assert until <= now + capture_control.MAX_PAUSE + timedelta(minutes=1)


def test_resume_clears_the_pause(tmp_path: Path) -> None:
    cli.main(["pause", "--out", str(tmp_path), "--minutes", "30"])
    assert capture_control.is_paused(tmp_path, datetime.now(UTC))
    rc = cli.main(["resume", "--out", str(tmp_path)])
    assert rc == 0
    assert capture_control.paused_until(tmp_path) is None
    assert not capture_control.is_paused(tmp_path, datetime.now(UTC))


def test_resume_when_not_paused_is_a_noop(tmp_path: Path) -> None:
    # Break-glass must be safe to run blindly ("just make sure it's recording").
    rc = cli.main(["resume", "--out", str(tmp_path)])
    assert rc == 0
    assert capture_control.paused_until(tmp_path) is None


def test_pause_then_resume_round_trips(tmp_path: Path) -> None:
    assert cli.main(["pause", "--out", str(tmp_path)]) == 0
    assert capture_control.is_paused(tmp_path, datetime.now(UTC))
    assert cli.main(["resume", "--out", str(tmp_path)]) == 0
    assert not capture_control.is_paused(tmp_path, datetime.now(UTC))


def test_parser_wires_pause_and_resume() -> None:
    p = build_parser()
    paused = p.parse_args(["pause", "--out", "d", "--minutes", "15"])
    assert paused.command == "pause"
    assert paused.minutes == 15
    assert paused.out == Path("d")
    resumed = p.parse_args(["resume", "--out", "d"])
    assert resumed.command == "resume"
    assert resumed.out == Path("d")


def test_pause_minutes_defaults_to_none() -> None:
    # None (not 0) so the handler falls back to MAX_PAUSE, matching capture_control.
    assert build_parser().parse_args(["pause"]).minutes is None
