"""Maintenance passes over the audio archive.

- `reprobe_short_segments`: repair rows that were indexed while their file was
  still being written (a partial file probes fine but short), so coverage,
  refine windows and trim clamps see the real duration again.

⚠ `compress_to_opus` lived here until 2026-09-11 and was DELETED, not disabled.
It re-encoded every non-Opus segment to 32 kbps and unlinked the original, which
under the retention decision of 2026-09-10 (lossless, forever) would destroy
exactly what the archive now exists to keep — and the phase information it took
with it cannot be recovered by anything. It was a manual subcommand, never
scheduled, so nothing ran it; a guard would have left a command whose whole
purpose is obsolete. Space comes back by filtering silence, not by re-encoding
speech.
"""

from __future__ import annotations

import subprocess
import time
from pathlib import Path

from recall.probe import probe_media
from recall.store import Store


def reprobe_short_segments(
    store: Store,
    *,
    max_expected_seconds: float = 58.0,
    min_age_seconds: float = 120.0,
    now: float | None = None,
) -> int:
    """Re-measure short-indexed segments and extend rows the files outgrew.

    A row indexed while capture was still writing its file recorded the partial
    duration permanently. Candidates are rows shorter than a normal segment
    (`max_expected_seconds`, just under the 60s ring); each is re-decoded and the
    stored end extended if the finalised file is longer. Genuinely short segments
    (capture stop/pause tails) re-measure the same and stay untouched. Files
    younger than `min_age_seconds` (possibly still being written) and missing
    files are skipped. Returns how many rows were repaired.
    """
    current = time.time() if now is None else now
    repaired = 0
    for audio_id, segment in store.short_audio_segments(
        max_seconds=max_expected_seconds
    ):
        path = Path(segment.path)
        try:
            if current - path.stat().st_mtime < min_age_seconds:
                continue
            duration, _, _ = probe_media(path)
        except (FileNotFoundError, subprocess.CalledProcessError, ValueError):
            continue
        end = segment.start + duration
        if end > segment.end:
            store.update_audio_segment_end(audio_id, end)
            repaired += 1
    return repaired
