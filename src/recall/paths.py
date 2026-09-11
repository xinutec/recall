"""Where the archive lives, resolved once for every entry point.

The data root used to be spelled `Path("data")` as each subcommand's `--out` default —
a relative path that exists on no machine that runs recall. A bare `recall doctor` or
`recall transcript` therefore opened an *empty* database in the working directory and
answered confidently about nothing: no error, no missing-file, just a wrong answer. That
cost real time (a doctor run once reported the backup "never completed" when it was
green), so the default now points at the archive the machine actually keeps.

`RECALL_OUT` wins when set (`recall api` exports it for the web stack). Otherwise the
root follows the role, because the two machines keep the archive in different places:
the fleet node (Isis) serves `/data` from its PVC; the Mac holds the master archive on
its external disk. Neither ever wanted `./data`.
"""

from __future__ import annotations

import os
from pathlib import Path

from recall.capture_control import is_fleet

# The fleet node's PVC mount; its deployment also passes `--out /data` explicitly.
FLEET_DATA_ROOT = Path("/data")
# The Mac's master archive — the external disk holding every recording.
MAC_DATA_ROOT = Path("/Volumes/Backup/recall")
# Where macOS mounts external disks — the only place a path implies a mount.
VOLUMES = Path("/Volumes")


def default_data_root() -> Path:
    """The archive root to use when the caller named none."""
    configured = os.environ.get("RECALL_OUT")
    if configured:
        return Path(configured)
    return FLEET_DATA_ROOT if is_fleet() else MAC_DATA_ROOT


class ArchiveAway(RuntimeError):
    """The archive root is not reachable — unmounted volume, or the wrong path."""


def require_archive(root: Path) -> Path:
    """The archive root, or `ArchiveAway` — and NOTHING is created either way.

    ⚠ **Never `mkdir(parents=True)` toward this path.** On the Mac the root is
    `/Volumes/Backup/recall`, so building it with parents asks macOS to create
    the MOUNTPOINT. `live.py` did exactly that, and `live.err.log` holds 562
    `PermissionError [Errno 13] '/Volumes/Backup'`. The EPERM is the only reason
    those were crashes rather than something worse: had the OS allowed it, every
    recording would have landed on the BOOT disk, under a directory the real
    volume then SHADOWS on remount — present, then gone, nothing logged.

    Two failure modes, so two checks:

    * **The root is absent.** An unmounted volume takes its mountpoint with it,
      so this is what a missing disk looks like.
    * **The root exists on the boot disk.** A mountpoint can survive an unmount
      as an empty directory, and that one is worse than a crash because writing
      to it succeeds. `st_dev` is the only thing that can tell the two apart —
      the path looks identical either way. Only checked under `/Volumes`, where
      mounting is what the path MEANS; `/data` on the fleet is a PVC and a
      `--out` under `/tmp` in a test is deliberately not a volume.

    What this does NOT require is that the archive already hold anything: a
    freshly mounted disk with no database yet is a first run, not a fault.

    Refusing loudly is the point. A `try/except` around the write would keep the
    hazard and hide the fault; a caller that cannot reach the archive has nothing
    useful to do except say so.
    """
    if not root.is_dir():
        raise ArchiveAway(
            f"archive not reachable at {root} — is the volume mounted? "
            "(nothing was created; see recall.paths.require_archive)"
        )
    if root.is_relative_to(VOLUMES) and os.stat(root).st_dev == os.stat("/").st_dev:
        raise ArchiveAway(
            f"{root} is on the BOOT DISK, not on its volume — an unmounted disk "
            "left its mountpoint behind. Refusing to write the archive there."
        )
    return root
