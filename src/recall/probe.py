"""Probe recorded segment files with ffprobe and reconstruct their timeline.

Segment start comes from the filename (UTC, embedded by ffmpeg's `-strftime`);
duration, sample rate, and channel count come from ffprobe. Together they give
the `Segment` objects that `recall.timeline` analyses for gaps.
"""

from __future__ import annotations

import json
import subprocess
import time
from datetime import timedelta
from pathlib import Path
from typing import Any

from recall.capture import parse_segment_start
from recall.timeline import Segment

_S16_BYTES_PER_SAMPLE = 2


def _decode_duration(path: Path, sample_rate: int, channels: int) -> timedelta:
    """Exact duration by decoding to raw PCM and counting bytes.

    Header-independent (segment-muxer FLAC carries no duration header) and exact
    at any length, including sub-second trailing segments where ffmpeg's
    human-readable progress reports `time=N/A`.
    """
    argv = [
        "ffmpeg",
        "-v",
        "error",
        "-i",
        str(path),
        "-f",
        "s16le",
        "-ac",
        str(channels),
        "-ar",
        str(sample_rate),
        "-",
    ]
    result = subprocess.run(argv, check=True, capture_output=True)
    frames = len(result.stdout) // (_S16_BYTES_PER_SAMPLE * channels)
    return timedelta(seconds=frames / sample_rate)


def probe_media(path: Path) -> tuple[timedelta, int, int]:
    """Return (duration, sample_rate, channels) for an audio file.

    Sample rate and channels come from the (reliable) stream header; duration is
    measured by decoding, since segment-muxer output lacks a duration header.
    """
    argv = [
        "ffprobe",
        "-v",
        "error",
        "-select_streams",
        "a:0",
        "-of",
        "json",
        "-show_entries",
        "stream=sample_rate,channels",
        str(path),
    ]
    result = subprocess.run(argv, check=True, capture_output=True, text=True)
    data: Any = json.loads(result.stdout)
    stream = data["streams"][0]
    sample_rate = int(stream["sample_rate"])
    channels = int(stream["channels"])
    return _decode_duration(path, sample_rate, channels), sample_rate, channels


def scan_segments(
    source_dir: Path,
    source_id: str,
    *,
    known: frozenset[str] = frozenset(),
    min_age_seconds: float = 0.0,
    now: float | None = None,
) -> list[Segment]:
    """Reconstruct the timeline of `source_id`'s segments under `source_dir`.

    Paths in `known` are skipped without probing — probing decodes the whole file,
    so re-probing the entire archive every pass doesn't scale. Pass the already-
    indexed paths to probe only new files.

    Files modified within `min_age_seconds` are skipped entirely: a partial
    Opus/FLAC still being written by capture probes FINE and yields its truncated
    duration — indexing it would record a short end permanently (the path lands in
    `known` and is never re-probed). Skipped files are picked up on a later scan,
    once finalised.
    """
    current = time.time() if now is None else now
    segments: list[Segment] = []
    for path in sorted(source_dir.glob(f"{source_id}-*")):
        if str(path) in known:
            continue
        try:
            too_young = current - path.stat().st_mtime < min_age_seconds
        except FileNotFoundError:
            continue  # vanished between glob and stat
        if too_young:
            continue
        start = parse_segment_start(path.name)
        try:
            duration, sample_rate, channels = probe_media(path)
        except (subprocess.CalledProcessError, ValueError):
            # Genuinely unreadable (e.g. zero-byte crash leftover) — skip rather
            # than crash the scan; retried next pass.
            continue
        segments.append(
            Segment(
                source_id=source_id,
                sequence=0,  # assigned after sorting below
                start=start,
                end=start + duration,
                path=str(path),
                sample_rate=sample_rate,
                channels=channels,
            )
        )
    segments.sort(key=lambda s: s.start)
    return [
        Segment(
            source_id=s.source_id,
            sequence=index,
            start=s.start,
            end=s.end,
            path=s.path,
            sample_rate=s.sample_rate,
            channels=s.channels,
        )
        for index, s in enumerate(segments)
    ]
