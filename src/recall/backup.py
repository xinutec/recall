"""Off-machine mirror of the archive, run through the recall CLI (python).

Why a python command and not a shell script: on macOS the external archive volume is
TCC-protected, and the grant is attached to the recall python process (the other agents
reach the volume through it). A launchd shell script running bare `/bin/mkdir` +
`/usr/bin/rsync` has no grant, so after a volume remount reset TCC every access was
denied and the nightly mirror silently stopped. Running the backup as `recall backup`
puts it in the same granted context as every other agent; its child `rsync` inherits it.

Two consistency rules (unchanged from the original shell design):
  1. The live SQLite file is never copied directly — a mid-write copy is garbage. A
     consistent snapshot is taken with sqlite's online-backup API and shipped as the
     mirror's `recall.sqlite`.
  2. rsync runs WITHOUT `--delete`: the remote is a superset, so a local catastrophe
     (or a bad edit) can propagate no deletions.
"""

from __future__ import annotations

import shutil
import sqlite3
import subprocess
from pathlib import Path

# Non-interactive SSH with a short connect timeout, matching the old agent.
DEFAULT_SSH = "ssh -o BatchMode=yes -o ConnectTimeout=15"

# Excluded from the audio rsync: the live DB (the snapshot replaces it), the
# transcode/diarize scratch, the staging dir, and the marker itself. The marker must
# reflect only LOCAL success, so a restored mirror must not carry a fresh-looking one.
_ARCHIVE_EXCLUDES = ("recall.sqlite*", "work/", ".backup-staging/", ".last-backup-ok")

_MARKER = ".last-backup-ok"


def rsync_argv(
    src: str, dest: str, *, ssh: str, excludes: tuple[str, ...] = ()
) -> list[str]:
    """The rsync command mirroring `src` to `dest` over `ssh`, archive mode, **without
    `--delete`** (the remote is a superset). Pure, so the exclude set is unit-tested."""
    argv = ["rsync", "-a", "-e", ssh]
    for pattern in excludes:
        argv += ["--exclude", pattern]
    argv += [src, dest]
    return argv


def snapshot_db(src: Path, dst: Path) -> None:
    """Write a consistent snapshot of the live SQLite DB to `dst`. Uses sqlite's online
    backup, so it is safe while the agents are writing (unlike a file copy)."""
    dst.parent.mkdir(parents=True, exist_ok=True)
    source = sqlite3.connect(f"file:{src}?mode=ro", uri=True, timeout=30)
    try:
        target = sqlite3.connect(str(dst), timeout=30)
        try:
            source.backup(target)
        finally:
            target.close()
    finally:
        source.close()


def run_backup(root: Path, dest: str, *, ssh: str = DEFAULT_SSH) -> None:
    """Mirror the archive at `root` to `dest`: consistent DB snapshot, then the audio
    (superset rsync), then touch the success marker. The staging dir lives inside the
    (granted, writable) archive root and is scratch — removed on exit."""
    staging = root / ".backup-staging"
    snapshot = staging / "recall.sqlite"
    staging.mkdir(parents=True, exist_ok=True)
    try:
        snapshot_db(root / "recall.sqlite", snapshot)
        subprocess.run(
            rsync_argv(f"{root}/", f"{dest}/", ssh=ssh, excludes=_ARCHIVE_EXCLUDES),
            check=True,
        )
        subprocess.run(
            rsync_argv(str(snapshot), f"{dest}/recall.sqlite", ssh=ssh), check=True
        )
        (root / _MARKER).touch()
    finally:
        shutil.rmtree(staging, ignore_errors=True)
