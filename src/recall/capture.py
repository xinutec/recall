"""ffmpeg segmentation/encoding of a raw PCM stream, plus filename helpers.

Capture reads a continuous raw-PCM stream from a source (see recall.sources) and
pipes it into ffmpeg's `segment` muxer, which writes a continuous ring of
fixed-length files — a crash loses at most one segment. Filenames embed a UTC
start timestamp (ffmpeg is run with TZ=UTC).

Only construction/parsing and the tiny shared file-layout helpers (the liveness
marker, the segment glob) live here; the capture pipelines themselves are
audiod's (docs/audio-plane.md).
"""

from __future__ import annotations

import re
from datetime import UTC, datetime
from pathlib import Path
from typing import Final

# ffmpeg -strftime token used in segment filenames, e.g. usb-20260613T140530.flac
_TS_STRFTIME: Final = "%Y%m%dT%H%M%S"
_TS_PARSE: Final = "%Y%m%dT%H%M%S"
_TS_RE: Final = re.compile(r"(\d{8}T\d{6})")

# Per-source liveness marker under the source's directory. Touched by whoever
# MEASURES the source delivering real signal — the ingest pump for a streaming
# phone, the metered producer→segmenter pump (plus the dead-segment watchdog's
# archive-level proof) for the local mic — never by mere process aliveness. Its
# freshness is what /api/sources calls "active": recording, not just connected
# (a green dot over a silent stream is how speech gets spoken into a
# not-recording window).
ALIVE_FILE: Final = ".alive"

# |s16| below this is digital silence, not a live mic: a real room's noise floor
# measures amplitude 10-90 (-69..-51 dB); a wedged CoreAudio read or the pixel9
# dead path yields exact zeros / amplitude 1. 2 tolerates dither while never
# calling a real, quiet room dead.
SILENCE_PEAK: Final = 2

# |s16 sample| at/above this counts as signal (~ -66 dBFS): safely above digital
# silence and codec dither (the pixel9 dead-path bug streams at amplitude ~1, -90 dB),
# safely below any live mic in a quiet room (~ -60..-50 dB).
_AUDIBLE_FLOOR: Final = 16
_S16_FULL_SCALE: Final = 32768


def segment_glob(source_dir: Path, source_id: str) -> list[Path]:
    """The source's segment files (any state: open, closed, stub), sorted by name —
    which is chronological, because the name embeds the UTC start time."""
    return sorted(source_dir.glob(f"{source_id}-*"))


# ffmpeg audio codec -> container file extension for segment files.
_CODEC_EXT: Final = {
    "flac": "flac",
    "pcm_s16le": "wav",
    "pcm_s24le": "wav",
    "libopus": "opus",
    "opus": "opus",
    "aac": "m4a",
}


def parse_segment_start(filename: str) -> datetime:
    """Parse the UTC start time embedded in a segment filename."""
    match = _TS_RE.search(filename)
    if match is None:
        msg = f"no timestamp found in segment filename {filename!r}"
        raise ValueError(msg)
    naive = datetime.strptime(match.group(1), _TS_PARSE)
    return naive.replace(tzinfo=UTC)
