"""Bound the agents' log files so they can't grow without limit.

The launchd agents append to `logs/<agent>.{out,err}.log` forever — over weeks that
reached ~1 GB, almost all of it library noise (model weight-loading bars, transformers
warnings). launchd has no rotation, and the agents hold their stdout/stderr open with
``O_APPEND``, so we can't rename the file out from under them. This does the classic
**copytruncate**: keep the last `keep_bytes` of an over-cap log in a `.1` sibling, then
truncate the original to zero — the writer keeps appending from offset 0, no reopen
needed. The trade-off (and copytruncate's only one) is that the few lines written during
the brief copy window can be lost; for noisy append-only logs that's fine.

Pure filesystem work (no models, no store), so it's unit-tested directly and called
cheaply from the always-on worker loop.
"""

from __future__ import annotations

import os
from pathlib import Path

# Defaults: cap each log at 16 MB, keeping the last 4 MB across a rotation. With ~14
# agent logs that bounds the directory to a couple hundred MB worst case, versus the
# unbounded ~1 GB it reached before.
DEFAULT_MAX_BYTES = 16 * 1024 * 1024
DEFAULT_KEEP_BYTES = 4 * 1024 * 1024


def rotate_logs(
    log_dir: Path,
    *,
    max_bytes: int = DEFAULT_MAX_BYTES,
    keep_bytes: int = DEFAULT_KEEP_BYTES,
) -> list[Path]:
    """copytruncate every ``*.log`` in `log_dir` that exceeds `max_bytes`.

    Returns the logs that were rotated. A missing directory or an unreadable file is
    skipped, never raised — rotation must never take down the worker.
    """
    if not log_dir.is_dir():
        return []
    rotated: list[Path] = []
    for log in sorted(log_dir.glob("*.log")):
        try:
            size = log.stat().st_size
            if size <= max_bytes:
                continue
            _copytruncate(log, keep_bytes=keep_bytes)
            rotated.append(log)
        except OSError:
            continue  # a transient FS error must not break the worker loop
    return rotated


def _copytruncate(log: Path, *, keep_bytes: int) -> None:
    with log.open("rb") as fh:
        size = fh.seek(0, os.SEEK_END)
        fh.seek(max(0, size - keep_bytes))
        tail = fh.read()
    # Drop a partial first line so the .1 starts clean (only when we actually trimmed).
    if size > keep_bytes and b"\n" in tail:
        tail = tail.split(b"\n", 1)[1]
    log.with_name(log.name + ".1").write_bytes(tail)
    os.truncate(log, 0)
