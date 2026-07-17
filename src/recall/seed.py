"""Seed the fleet's system of record from the Mac's archive — the one-time migration.

`recall.sync` is the *delta* protocol: it carries new machine turns to a fleet that
already holds the archive. It is the wrong tool for the move itself, and using it would
have quietly lost most of what matters. Measured on the real archive before the move:

    sync_push would have sent   1,440 of 2,252 segments
    it would NOT have sent        812 segments (their audio: gone)
    of those, carrying a human correction: 47
    and it carries no human turns at all — 525 of them
    nor the corrections table (468 rows — the training corpus: original vs corrected)
    nor transcript_embeddings  (22,727), speaker_embeddings (750), or the FTS index

None of that is derivable from what sync sends. The audio can be re-transcribed; a
person's correction cannot be re-made, and the whole point of moving the system of
record off the Mac is that the Mac may die.

So the migration copies the database — every table, every row, lineage included — and
rewrites the one thing that cannot survive the trip: where the audio lives.

**The path column.** `audio_segments.path` is absolute on the machine that recorded
it (`/Volumes/Backup/recall/usb/…`). On the fleet the same audio sits under the fleet's
own root (`/data/usb/…`). Copied verbatim, the fleet would hold a database describing a
filesystem it cannot see: transcripts perfect, every play button dead.

**What is NOT rewritten.** `transcript_segments.asr_model` also holds strings that look
like paths (`/Volumes/Backup/recall/adapter-current`) — but they are the *name of the
model* that produced a turn, not a file the fleet opens. They are historical fact and
are left as they are; rewriting them would forge the provenance of 290 turns.
"""

from __future__ import annotations

import shutil
import sqlite3
from dataclasses import dataclass
from pathlib import Path

# The archive root inside the fleet container (the PVC mount; see deploy/k8s).
FLEET_ROOT = "/data"


def fleet_path(mac_path: str, source_id: str, fleet_root: str = FLEET_ROOT) -> str:
    """Where a segment recorded at `mac_path` will live on the fleet.

    Only the filename survives: the fleet owns its layout (the same rule `recall.sync`
    applies to a pushed segment), and the sender's directories are none of its business.
    """
    return f"{fleet_root}/{source_id}/{Path(mac_path).name}"


@dataclass(frozen=True)
class Seed:
    """What the seed contains, so the migration can be checked rather than trusted."""

    segments: int
    rewritten: int
    missing_audio: tuple[str, ...]  # rows whose audio file is not on disk


def build_seed(
    live_db: Path, seed_db: Path, *, archive_root: Path, fleet_root: str = FLEET_ROOT
) -> Seed:
    """Snapshot the live database and re-home its audio paths for the fleet.

    A snapshot, never a file copy: the live database is in WAL mode and is being
    written by the worker as this runs, so copying the file would ship a torn page.
    `sqlite3.backup` takes a consistent point-in-time image of a database in use — the
    same reason odin's nightly backup-prepare takes its snapshot this way.

    Every rewritten row is checked against the audio actually on disk. A row pointing at
    a file that is not there is reported, not silently carried: on the fleet it would be
    a recording that exists in the index and nowhere else.
    """
    seed_db.parent.mkdir(parents=True, exist_ok=True)
    if seed_db.exists():
        seed_db.unlink()

    source = sqlite3.connect(f"file:{live_db}?mode=ro", uri=True)
    try:
        target = sqlite3.connect(seed_db)
        try:
            source.backup(target)  # consistent, even mid-write
        finally:
            target.close()
    finally:
        source.close()

    db = sqlite3.connect(seed_db)
    try:
        rows = db.execute("SELECT id, source_id, path FROM audio_segments").fetchall()
        missing: list[str] = []
        rewritten = 0
        for audio_id, source_id, path in rows:
            if not (archive_root / source_id / Path(path).name).exists():
                missing.append(str(path))
                continue
            db.execute(
                "UPDATE audio_segments SET path = ? WHERE id = ?",
                (fleet_path(str(path), str(source_id), fleet_root), audio_id),
            )
            rewritten += 1
        db.commit()
    finally:
        db.close()

    return Seed(segments=len(rows), rewritten=rewritten, missing_audio=tuple(missing))


def copy_audio(archive_root: Path, staging: Path, sources: list[str]) -> int:
    """Stage the audio the seed refers to, laid out as the fleet expects it."""
    copied = 0
    for source_id in sources:
        src_dir = archive_root / source_id
        if not src_dir.is_dir():
            continue
        dest_dir = staging / source_id
        dest_dir.mkdir(parents=True, exist_ok=True)
        for path in src_dir.iterdir():
            if path.is_file():
                shutil.copy2(path, dest_dir / path.name)
                copied += 1
    return copied
