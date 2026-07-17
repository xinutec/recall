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


def default_data_root() -> Path:
    """The archive root to use when the caller named none."""
    configured = os.environ.get("RECALL_OUT")
    if configured:
        return Path(configured)
    return FLEET_DATA_ROOT if is_fleet() else MAC_DATA_ROOT
