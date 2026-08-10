"""The worker's pulse — proof that a pass happened, whether or not it found work.

The worker prints only when it writes transcript rows, which means a quiet house
and a wedged pipeline produce exactly the same log: none. On 2026-08-10
`worker.out.log` had not been written for three days, and for over an hour of
that the worker was in uninterruptible disk wait with an hour of captured audio
piling up behind it. Nothing on the machine could tell those two apart (#709).

So the worker stamps a file at the start and end of every pass. It advances every
ten seconds while there is nothing to do, and stops the moment a pass cannot
finish — which makes "when did a pass last complete" the whole question, and one
`recall doctor` can answer.

It lives in the archive root rather than off it, deliberately: a heartbeat that
kept ticking while the archive was unreachable would be worse than none, since
the thing it certifies is work done *on* that archive. When the volume is
unreadable the doctor says so directly (`recall.bounded`, `health.archive_check`)
and this check is not the one to duplicate it.
"""

from __future__ import annotations

import json
import os
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Final

FILENAME: Final = "worker-heartbeat.json"


@dataclass(frozen=True)
class Beat:
    """One pass of the worker, stamped when it began and again when it ended.

    `finished is None` means the pass is still running — the distinction between
    "the loop has stopped starting passes" and "a pass has stopped returning",
    which point at launchd and at the archive respectively.
    """

    started: datetime
    finished: datetime | None
    seconds: float | None
    rows: int


def path(root: Path) -> Path:
    return root / FILENAME


def write(root: Path, beat: Beat) -> None:
    """Stamp the beat, atomically.

    The doctor reads this every five minutes and the worker rewrites it every ten
    seconds, so a non-atomic write is not a rare race but a scheduled one: write
    beside it and `os.replace`, which is atomic within a filesystem.
    """
    payload = {
        "started": beat.started.isoformat(),
        "finished": None if beat.finished is None else beat.finished.isoformat(),
        "seconds": beat.seconds,
        "rows": beat.rows,
    }
    target = path(root)
    scratch = target.with_suffix(".tmp")
    scratch.write_text(json.dumps(payload), encoding="utf-8")
    os.replace(scratch, target)


def read(root: Path) -> Beat | None:
    """The last stamped pass, or None if there is none to read.

    ⚠ Never raises, and never guesses. This is read by the health check, so a
    missing file must not take the doctor down and a damaged one must not be
    coerced into a timestamp — "no beat" is a verdict the check knows how to
    grade, while a wrong time reads as healthy.
    """
    try:
        raw = json.loads(path(root).read_text(encoding="utf-8"))
        finished = raw["finished"]
        return Beat(
            started=datetime.fromisoformat(raw["started"]).astimezone(UTC),
            finished=None
            if finished is None
            else datetime.fromisoformat(finished).astimezone(UTC),
            seconds=None if raw["seconds"] is None else float(raw["seconds"]),
            rows=int(raw["rows"]),
        )
    except (OSError, json.JSONDecodeError, LookupError, TypeError, ValueError):
        return None
