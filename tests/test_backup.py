"""Off-machine mirror: the rsync command shape and the consistent DB snapshot.

The volume/SSH side effects aren't unit-tested (they need the real hosts); the parts
that can silently go wrong — a stray ``--delete``, a missing exclude, or a snapshot that
doesn't actually capture the rows — are."""

from __future__ import annotations

import sqlite3
from pathlib import Path

from recall.backup import _ARCHIVE_EXCLUDES, rsync_argv, snapshot_db


def test_rsync_argv_never_deletes() -> None:
    argv = rsync_argv("/src/", "host:/dest/", ssh="ssh -o BatchMode=yes")
    assert "--delete" not in argv  # the remote is a superset; deletions never propagate
    assert argv[:4] == ["rsync", "-a", "-e", "ssh -o BatchMode=yes"]
    assert argv[-2:] == ["/src/", "host:/dest/"]


def test_rsync_argv_carries_every_exclude() -> None:
    argv = rsync_argv("/src/", "host:/dest/", ssh="ssh", excludes=_ARCHIVE_EXCLUDES)
    # the live DB, scratch, staging, and the marker are all excluded from the audio push
    for pattern in ("recall.sqlite*", "work/", ".backup-staging/", ".last-backup-ok"):
        assert argv.count("--exclude") == len(_ARCHIVE_EXCLUDES)
        i = argv.index(pattern)
        assert argv[i - 1] == "--exclude"


def test_snapshot_db_captures_the_rows(tmp_path: Path) -> None:
    src = tmp_path / "live.sqlite"
    con = sqlite3.connect(str(src))
    con.execute("CREATE TABLE t (id INTEGER, txt TEXT)")
    con.executemany("INSERT INTO t VALUES (?, ?)", [(1, "a"), (2, "b"), (3, "c")])
    con.commit()
    con.close()

    dst = tmp_path / "snap" / "recall.sqlite"
    snapshot_db(src, dst)

    assert dst.exists()
    snap = sqlite3.connect(str(dst))
    assert snap.execute("SELECT count(*) FROM t").fetchone()[0] == 3
    assert snap.execute("PRAGMA integrity_check").fetchone()[0] == "ok"
    snap.close()
