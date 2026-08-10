"""The doctor must be able to report a volume it cannot read.

On 2026-08-10 it could not. An unrelated bulk delete starved `/Volumes/Backup`
for over an hour and `recall-doctor` — which lives on that volume and reads it —
went into uninterruptible disk wait alongside the worker, the refiner and the
sync. With `KeepAlive = false` and a 300s `StartInterval`, launchd starts no
further run while one is stuck, so a single wedged doctor silenced every doctor
after it, and the hour of unregistered audio was found by hand (#709, diagnosed
in #701).

So the split under test is: everything that READS the archive happens in a child
process the parent abandons, and everything that REPORTS happens in a parent that
touches only launchd and `~/.config`.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from recall import bounded, cli


def _unanswered(*_args: object, **_kwargs: object) -> bounded.Answer:
    return bounded.Answer(
        stdout=None, stderr="", returncode=None, seconds=60.0, pid=4242
    )


def test_the_archive_is_read_by_a_child_and_its_checks_come_back(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    """End to end, with a real child process against a real (empty) archive.

    This also pins the child to the same build as the parent: the installed wheel
    predates `--collect`, so a child that resolved `recall` from site-packages
    instead of from here would exit 2 and turn the whole archive section into one
    failure.
    """
    cli.main(["doctor", "--out", str(tmp_path)])
    out = capsys.readouterr().out
    assert "archive/archive answers: read in " in out
    assert "capture/recording" in out, f"the child's checks did not come back:\n{out}"


def test_an_unreachable_archive_is_a_verdict_rather_than_a_hang(
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(bounded, "run", _unanswered)
    rc = cli.main(["doctor", "--out", str(tmp_path)])
    captured = capsys.readouterr()

    assert rc == 1
    assert "archive/archive answers: no answer in 60s" in captured.out
    # ⚠ FAIL, never skip: a skip reads as "not applicable", and the whole finding
    # is that the archive is unreachable and very much applicable.
    assert "[FAIL] archive/archive answers" in captured.out.replace("  ", " ")


def test_the_abandoned_child_is_named_so_it_can_be_found_in_ps(
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The pid is the only handle on a process nothing can kill.

    A child in uninterruptible wait outlives the doctor that gave up on it, and
    an operator looking at a stalled machine needs to know which `U`-state
    process was ours and which belongs to the delete that caused it.
    """
    monkeypatch.setattr(bounded, "run", _unanswered)
    cli.main(["doctor", "--out", str(tmp_path)])
    assert "4242" in capsys.readouterr().err


def test_a_wedged_archive_does_not_take_the_rest_of_the_report_with_it(
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The agents still get reported: they are read from launchd, off the volume.

    This is the property that makes the split worth the child process. Reporting
    nothing is what the old doctor did, and fleetwatch cannot tell "the archive
    is unreadable" from "the Mac is gone" if the whole report stops.
    """
    monkeypatch.setattr(bounded, "run", _unanswered)
    cli.main(["doctor", "--out", str(tmp_path)])
    assert "agents/" in capsys.readouterr().out


def test_a_child_that_dies_says_why(
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    def crashed(*_args: object, **_kwargs: object) -> bounded.Answer:
        return bounded.Answer(
            stdout="",
            stderr=(
                "Traceback...\nsqlite3.DatabaseError: database disk image is malformed"
            ),
            returncode=1,
            seconds=2.0,
            pid=7,
        )

    monkeypatch.setattr(bounded, "run", crashed)
    rc = cli.main(["doctor", "--out", str(tmp_path)])
    out = capsys.readouterr().out
    assert rc == 1
    assert "database disk image is malformed" in out
    assert "after 2.0s" in out


def test_collect_emits_json_and_nothing_else(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    """`--collect`'s stdout is a protocol, not a display.

    One stray `print` in the archive half becomes an unparseable report, which
    the parent can only render as a failed archive — a self-inflicted outage
    with no way to tell it from a real one.
    """
    rc = cli.main(["doctor", "--out", str(tmp_path), "--collect"])
    out = capsys.readouterr().out
    assert rc == 0
    report = json.loads(out)
    assert report["checks"], "the child reported no checks"
    assert isinstance(report["seconds"], float)
    assert "doctor:" not in out


def test_the_child_times_itself_rather_than_its_own_startup(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    """~1.3s of imports would swamp the reading this is meant to trend.

    The archive figure has to be the archive: a second interpreter's startup is
    a constant that would sit on top of every sample and hide the range that
    matters, which on this disk is tens of milliseconds warm against seconds
    when something else owns the queue.
    """
    cli.main(["doctor", "--out", str(tmp_path), "--collect"])
    report = json.loads(capsys.readouterr().out)
    assert report["seconds"] < 1.0
