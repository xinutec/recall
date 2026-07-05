"""Log rotation: copytruncate over-cap logs, leave small ones, never crash."""

from __future__ import annotations

import os
from pathlib import Path

import pytest

from recall import cli
from recall.logrotate import DEFAULT_MAX_BYTES, rotate_logs


def _write(path: Path, size: int) -> None:
    path.write_bytes(b"x" * size)


def test_rotates_only_over_cap_logs(tmp_path: Path) -> None:
    big = tmp_path / "api.err.log"
    small = tmp_path / "api.out.log"
    _write(big, 5000)
    _write(small, 100)

    rotated = rotate_logs(tmp_path, max_bytes=1000, keep_bytes=500)

    assert rotated == [big]
    # The big log is truncated to (about) keep_bytes, its tail preserved in .1.
    assert big.stat().st_size <= 500
    assert (tmp_path / "api.err.log.1").exists()
    assert (tmp_path / "api.err.log.1").stat().st_size <= 500
    # The small log is untouched, and gets no .1.
    assert small.stat().st_size == 100
    assert not (tmp_path / "api.out.log.1").exists()


def test_keeps_the_tail_not_the_head(tmp_path: Path) -> None:
    log = tmp_path / "worker.err.log"
    log.write_bytes(b"OLD-HEAD\n" + b"a" * 2000 + b"\nNEW-TAIL\n")

    rotate_logs(tmp_path, max_bytes=500, keep_bytes=300)

    kept = (tmp_path / "worker.err.log.1").read_bytes()
    assert b"NEW-TAIL" in kept
    assert b"OLD-HEAD" not in kept


def test_truncated_log_keeps_appending_from_zero(tmp_path: Path) -> None:
    # Simulate launchd's O_APPEND writer surviving the truncation: after rotation the
    # same fd keeps writing, and lands at the new (zero) offset — no sparse hole.
    log = tmp_path / "live.out.log"
    _write(log, 4000)
    fd = os.open(log, os.O_WRONLY | os.O_APPEND)
    try:
        rotate_logs(tmp_path, max_bytes=1000, keep_bytes=500)
        os.write(fd, b"after\n")
    finally:
        os.close(fd)
    assert log.read_bytes() == b"after\n"


def test_missing_dir_is_a_noop(tmp_path: Path) -> None:
    assert rotate_logs(tmp_path / "nope") == []


def test_cli_main_rotates_logs_on_any_entry(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    # Rotation must not depend on the worker being alive: when the worker is down
    # (unmounted volume, crash-loop) the other agents' logs previously grew
    # unbounded. Every CLI entry now rotates over-cap logs on start.
    logs = tmp_path / "logs"
    logs.mkdir()
    big = logs / "recall-api.err.log"
    big.write_bytes(b"x" * (DEFAULT_MAX_BYTES + 1))
    monkeypatch.setattr(cli, "_LOG_DIR", logs)

    assert cli.main(["reprobe", "--out", str(tmp_path)]) == 0
    capsys.readouterr()

    assert big.stat().st_size == 0  # truncated
    assert (logs / "recall-api.err.log.1").exists()  # tail kept
