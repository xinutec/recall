"""The archive root must be FOUND, never created — and never on the boot disk."""

from __future__ import annotations

import os
from pathlib import Path

import pytest

from recall import paths
from recall.paths import ArchiveAway, require_archive


def test_a_reachable_archive_is_returned_unchanged(tmp_path: Path) -> None:
    assert require_archive(tmp_path) == tmp_path


def test_a_fresh_disk_with_no_database_yet_is_a_first_run_not_a_fault(
    tmp_path: Path,
) -> None:
    """A newly mounted archive holds nothing. Requiring `recall.sqlite` here
    would make the first run on any machine impossible."""
    assert not (tmp_path / "recall.sqlite").exists()

    assert require_archive(tmp_path) == tmp_path


def test_an_absent_root_refuses_and_creates_nothing(tmp_path: Path) -> None:
    """⚠ The fault this exists for: `mkdir(parents=True)` on
    `/Volumes/Backup/recall/work` asks macOS to create the MOUNTPOINT. 562 of
    those are in live.err.log, each an EPERM — and EPERM is the only reason they
    were crashes rather than a silent write to the boot disk, which the real
    volume would then SHADOW on remount."""
    away = tmp_path / "Volumes" / "Backup" / "recall"

    with pytest.raises(ArchiveAway, match=str(away)):
        require_archive(away)

    assert not away.exists()
    assert not away.parent.exists(), "the mountpoint itself must not be created"


def test_a_mountpoint_left_behind_on_the_boot_disk_is_refused(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """An unmount can leave the mountpoint behind as an empty directory. That is
    the WORSE fault, because writing to it succeeds — the path is identical, so
    only the device number can tell them apart."""
    stale = tmp_path / "Backup" / "recall"
    stale.mkdir(parents=True)
    # Stand `VOLUMES` up over the temp tree: `tmp_path` is on the boot disk,
    # which is exactly the condition being detected.
    monkeypatch.setattr(paths, "VOLUMES", tmp_path)
    assert os.stat(stale).st_dev == os.stat("/").st_dev

    with pytest.raises(ArchiveAway, match="BOOT DISK"):
        require_archive(stale)


def test_a_root_outside_volumes_is_not_judged_on_its_device(tmp_path: Path) -> None:
    """`/data` on the fleet is a PVC and a test's `--out` is a temp directory.
    Neither is a mount in the macOS sense, and requiring one would refuse both."""
    assert require_archive(tmp_path) == tmp_path
