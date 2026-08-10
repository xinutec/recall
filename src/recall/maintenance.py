"""Maintenance passes over the audio archive.

- `compress_to_opus`: re-encode non-Opus segments to Opus, verified decodable,
  re-linked in the store, original removed. Transcripts/corrections reference
  segments by id (not path), so they're unaffected.
- `reprobe_short_segments`: repair rows that were indexed while their file was
  still being written (a partial file probes fine but short), so coverage,
  refine windows and trim clamps see the real duration again.
"""

from __future__ import annotations

import subprocess
import time
from pathlib import Path

from recall.probe import probe_media
from recall.store import Store


def compress_to_opus(
    store: Store, *, bitrate: str = "32k", remove_original: bool = True
) -> tuple[int, int]:
    """Transcode non-Opus segments to Opus. Returns (count, bytes_reclaimed)."""
    count = 0
    reclaimed = 0
    for audio_id, path_str in store.audio_segment_paths():
        path = Path(path_str)
        if path.suffix == ".opus" or not path.exists():
            continue
        opus = path.with_suffix(".opus")
        subprocess.run(
            [
                "ffmpeg",
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-i",
                str(path),
                "-c:a",
                "libopus",
                "-b:a",
                bitrate,
                "-application",
                "voip",
                str(opus),
            ],
            check=True,
        )
        probe_media(opus)  # verify decodable before removing the original
        store.relink_audio_segment(audio_id, str(opus))
        if remove_original:
            reclaimed += path.stat().st_size - opus.stat().st_size
            path.unlink()
        count += 1
    return count, reclaimed


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
