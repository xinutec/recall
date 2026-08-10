"""Probe recorded segment files with ffprobe and reconstruct their timeline.

Segment start comes from the filename (UTC, embedded by ffmpeg's `-strftime`);
duration, sample rate, and channel count come from ffprobe. Together they give
the `Segment` objects that `recall.timeline` analyses for gaps.
"""

from __future__ import annotations

import json
import subprocess
import time
from dataclasses import dataclass
from datetime import timedelta
from pathlib import Path
from typing import Any, NamedTuple

from recall.capture import parse_segment_start, segment_glob
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
        "-nostdin",
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


class MediaInfo(NamedTuple):
    duration: timedelta
    sample_rate: int
    channels: int


def probe_media(path: Path) -> MediaInfo:
    """What an audio file holds. Sample rate and channels come from the (reliable)
    stream header; duration is measured by decoding, since segment-muxer output
    lacks a duration header."""
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
    duration = _decode_duration(path, sample_rate, channels)
    return MediaInfo(duration, sample_rate, channels)


@dataclass(frozen=True)
class Scan:
    """What a pass over one source's directory found — including what it could not read.

    The unreadable files used to be dropped on the floor with a `continue` and the note
    "retried next pass". They were: 46 zero-byte files sat in this archive from June,
    re-probed (an ffprobe *and* a full decode, each) on every worker pass since, and
    nothing ever said so. A file the pipeline cannot read is a fact about the archive,
    not a thing to silently skip forever.
    """

    segments: list[Segment]
    # Zero bytes: capture opened the file and wrote nothing to it — it died or was
    # killed on the spot. It holds no audio and never will (a segment path carries its
    # own timestamp, so capture never reopens one). It is a tombstone, not a recording.
    empty: list[Path]
    # Non-empty, but ffprobe refuses it: truncated mid-write, or corrupt. This one may
    # still hold audio, so it is only reported — never removed.
    unreadable: list[Path]


def scan_source(
    source_dir: Path,
    source_id: str,
    *,
    known: frozenset[str] = frozenset(),
    min_age_seconds: float = 0.0,
    now: float | None = None,
) -> Scan:
    """Reconstruct the timeline of `source_id`'s segments under `source_dir`.

    Paths in `known` are skipped without probing — probing decodes the whole file,
    so re-probing the entire archive every pass doesn't scale. Pass the already-
    indexed paths to probe only new files.

    Files modified within `min_age_seconds` are skipped entirely: a partial
    Opus/FLAC still being written by capture probes FINE and yields its truncated
    duration — indexing it would record a short end permanently (the path lands in
    `known` and is never re-probed). Skipped files are picked up on a later scan,
    once finalised. That guard is also what makes an empty file safe to call dead: one
    still being written is younger than the bar and never reaches this.
    """
    current = time.time() if now is None else now
    segments: list[Segment] = []
    empty: list[Path] = []
    unreadable: list[Path] = []
    for path in segment_glob(source_dir, source_id):
        if str(path) in known:
            continue
        try:
            stat = path.stat()
        except FileNotFoundError:
            continue  # vanished between glob and stat
        if current - stat.st_mtime < min_age_seconds:
            continue
        if stat.st_size == 0:
            # Not probed: there is nothing to probe. Two subprocesses saved per file
            # per pass, and — more to the point — it gets *reported* instead of being
            # skipped in silence.
            empty.append(path)
            continue
        start = parse_segment_start(path.name)
        try:
            duration, sample_rate, channels = probe_media(path)
        except (subprocess.CalledProcessError, ValueError):
            unreadable.append(path)
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
    return Scan(
        segments=[
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
        ],
        empty=empty,
        unreadable=unreadable,
    )


def scan_segments(
    source_dir: Path,
    source_id: str,
    *,
    known: frozenset[str] = frozenset(),
    min_age_seconds: float = 0.0,
    now: float | None = None,
) -> list[Segment]:
    """Just the readable segments (see `scan_source`) — for callers rebuilding a
    timeline, who have nothing useful to do about a file that will not open."""
    return scan_source(
        source_dir,
        source_id,
        known=known,
        min_age_seconds=min_age_seconds,
        now=now,
    ).segments
