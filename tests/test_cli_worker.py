"""The worker leaves a pulse even when it has nothing to say.

That is the whole point: `worker.out.log` is written only when a pass produces
transcript rows, so the quiet case — which is most cases — left no trace at all,
and an hour of unindexed audio behind a wedged archive looked exactly like a
quiet house (#709).
"""

from __future__ import annotations

from datetime import UTC, datetime
from pathlib import Path

from recall import cli, heartbeat
from recall.health import worker_check


def test_a_pass_that_found_nothing_still_leaves_a_pulse(tmp_path: Path) -> None:
    assert heartbeat.read(tmp_path) is None, "nothing has run yet"

    rc = cli.main(["worker", "--out", str(tmp_path), "--basic"])
    assert rc == 0

    beat = heartbeat.read(tmp_path)
    assert beat is not None, "a completed pass left no heartbeat"
    assert beat.finished is not None, "the pass was stamped as still running"
    assert beat.rows == 0
    assert beat.seconds is not None and beat.seconds >= 0.0
    assert beat.started <= beat.finished


def test_the_pulse_a_real_pass_leaves_reads_as_healthy(tmp_path: Path) -> None:
    """End to end: what the worker writes is what the doctor grades.

    The two halves are in different modules and were written apart, so a field
    the check reads and the worker never sets would show up as a permanently
    failing pipeline rather than as a bug.
    """
    cli.main(["worker", "--out", str(tmp_path), "--basic"])
    check = worker_check(heartbeat.read(tmp_path), now=datetime.now(UTC))
    assert check.verdict == "pass", check.observed
