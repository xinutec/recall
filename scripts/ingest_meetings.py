#!/usr/bin/env python3
"""Ingest discrete meeting recordings (e.g. doctor consultations) into a recall root.

Filenames carry the local (Europe/London) start time as `YYYY_MM_DD_HH_MM_SS_N.mp3`.
We parse that, probe the duration, fold short add-ons into the recording they continue
(recall.sessions), and register one source per session — each file kept at its true
time. Times are stored in UTC (as everywhere in recall); source ids are labelled with
local time, which is how you think of the meeting ("the 19:01 one").

Run:  nix develop --command .venv/bin/python scripts/ingest_meetings.py
"""

from __future__ import annotations

import glob
import shutil
import sys
from datetime import UTC, datetime
from pathlib import Path
from zoneinfo import ZoneInfo

from recall.probe import probe_media
from recall.sessions import Recording, group_into_sessions
from recall.sources import AudioSource, SourceKind
from recall.store import Store
from recall.timeline import Segment

LONDON = ZoneInfo("Europe/London")
# Usage: ingest_meetings.py [target-root] [incoming-dir]
_argv = sys.argv[1:]
_DEFAULT_INCOMING = Path("/Volumes/Backup/recall-meetings/incoming")
ROOT = Path(_argv[0]) if _argv else Path("/Volumes/Backup/recall")
INCOMING = Path(_argv[1]) if _argv[1:] else _DEFAULT_INCOMING


def _parse_start(filename: str) -> datetime:
    """`2026_05_20_19_01_21_1.mp3` -> 2026-05-20 19:01:21 London, as UTC."""
    parts = Path(filename).stem.split("_")
    y, mo, d, h, mi, s = (int(x) for x in parts[:6])
    return datetime(y, mo, d, h, mi, s, tzinfo=LONDON).astimezone(UTC)


def main() -> None:
    probed: dict[str, tuple[Path, int, int]] = {}  # filename -> (path, rate, channels)
    recordings: list[Recording] = []
    for path_str in glob.glob(str(INCOMING / "*.mp3")):
        path = Path(path_str)
        start = _parse_start(path.name)
        duration, sample_rate, channels = probe_media(path)
        probed[path.name] = (path, sample_rate, channels)
        recordings.append(Recording(key=path.name, start=start, end=start + duration))

    sessions = group_into_sessions(recordings)
    store = Store.open(ROOT / "recall.sqlite")
    try:
        for session in sessions:
            anchor = session[0]
            local = anchor.start.astimezone(LONDON)
            source_id = f"meeting-{local:%Y%m%d-%H%M}"
            name = f"Meeting {local:%Y-%m-%d %H:%M}"
            # Authoritative, so re-running over a directory the worker already
            # discovered repairs it rather than leaving the guess in place.
            store.register_source(
                AudioSource(id=source_id, name=name, kind=SourceKind.UPLOAD, spec="")
            )
            for sequence, rec in enumerate(session):
                src_path, sample_rate, channels = probed[rec.key]
                out_dir = ROOT / source_id
                out_dir.mkdir(parents=True, exist_ok=True)
                stamp = rec.start.astimezone(UTC).strftime("%Y%m%dT%H%M%S")
                dest = out_dir / f"{source_id}-{stamp}.mp3"
                shutil.copy(src_path, dest)
                store.add_audio_segment(
                    Segment(
                        source_id=source_id,
                        sequence=sequence,
                        start=rec.start,
                        end=rec.end,
                        path=str(dest),
                        sample_rate=sample_rate,
                        channels=channels,
                    )
                )
            mins = "+".join(
                str(round((r.end - r.start).total_seconds() / 60)) for r in session
            )
            print(f"{source_id:24s} {len(session)} file(s)  {mins} min")
    finally:
        store.close()
    print(f"\n{len(recordings)} files -> {len(sessions)} sessions ingested into {ROOT}")


if __name__ == "__main__":
    main()
