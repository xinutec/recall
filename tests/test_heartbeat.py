"""Telling an idle worker from a stuck one.

`worker.out.log` is written only when a pass produces transcript rows, so on
2026-08-10 it had not been touched since the 7th — and that is what a healthy
quiet house looks like too. The pipeline had in fact been in uninterruptible disk
wait for over an hour with an hour of captured audio unindexed behind it, and
nothing anywhere could say which of the two it was (#709).

A heartbeat is the smallest thing that separates them: it advances while the
worker is doing nothing, and stops when the worker cannot.
"""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from pathlib import Path

from recall.heartbeat import Beat, path, read, write

NOW = datetime(2026, 8, 10, 12, 0, 0, tzinfo=UTC)


def _beat(**over: object) -> Beat:
    fields: dict[str, object] = {
        "started": NOW,
        "finished": NOW + timedelta(seconds=2),
        "seconds": 2.0,
        "rows": 3,
    }
    fields.update(over)
    return Beat(**fields)  # type: ignore[arg-type]


def test_a_written_beat_reads_back_unchanged(tmp_path: Path) -> None:
    write(tmp_path, _beat())
    assert read(tmp_path) == _beat()


def test_a_pass_still_running_reads_back_as_unfinished(tmp_path: Path) -> None:
    write(tmp_path, _beat(finished=None, seconds=None, rows=0))
    beat = read(tmp_path)
    assert beat is not None
    assert beat.finished is None
    assert beat.started == NOW


def test_no_heartbeat_at_all_is_not_an_error(tmp_path: Path) -> None:
    # A fresh archive has never run a pass. That is a finding for the check to
    # grade, not an exception for the doctor to die on.
    assert read(tmp_path) is None


def test_a_torn_or_corrupt_heartbeat_reads_as_absent(tmp_path: Path) -> None:
    """Never a crash, and never a wrong time.

    The reader is the health check; a half-written file must not take it down,
    and must not be parsed into a stale timestamp that reads as healthy.
    """
    write(tmp_path, _beat())
    path(tmp_path).write_text('{"started": "not-a-time"', encoding="utf-8")
    assert read(tmp_path) is None


def test_a_beat_replaces_the_last_one_atomically(tmp_path: Path) -> None:
    """`os.replace`, so a doctor never sees a half-written pass.

    The reader runs every 5 minutes against a file rewritten every 10 seconds, so
    a non-atomic write is not a rare race — it is a scheduled one.
    """
    write(tmp_path, _beat())
    later = _beat(started=NOW + timedelta(minutes=1), rows=9)
    write(tmp_path, later)
    assert read(tmp_path) == later
    assert len(list(tmp_path.iterdir())) == 1, "the temp file was left behind"
