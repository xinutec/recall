"""ffmpeg segmentation/encoding of a raw PCM stream, plus filename helpers.

Capture reads a continuous raw-PCM stream from a source (see recall.sources) and
pipes it into ffmpeg's `segment` muxer, which writes a continuous ring of
fixed-length files — a crash loses at most one segment. Filenames embed a UTC
start timestamp (ffmpeg is run with TZ=UTC).

Only construction/parsing and the tiny shared file-layout helpers (the liveness
marker, the segment glob) live here; running the pipe is in recall.runner.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Final

from recall.sources import fanout_output_argv

# ffmpeg -strftime token used in segment filenames, e.g. usb-20260613T140530.flac
_TS_STRFTIME: Final = "%Y%m%dT%H%M%S"
_TS_PARSE: Final = "%Y%m%dT%H%M%S"
_TS_RE: Final = re.compile(r"(\d{8}T\d{6})")

# Per-source liveness marker under the source's directory. Touched by whoever
# MEASURES the source delivering real signal — the ingest pump for a streaming
# phone, the dead-segment watchdog for the local mic — never by mere process
# aliveness. Its freshness is what /api/sources calls "active": recording, not
# just connected (docs/capture-loss-plan.md — a green dot over a silent stream
# is how speech gets spoken into a not-recording window).
ALIVE_FILE: Final = ".alive"

# |s16| below this is digital silence, not a live mic: a real room's noise floor
# measures amplitude 10-90 (-69..-51 dB); a wedged CoreAudio read or the pixel9
# dead path yields exact zeros / amplitude 1. 2 tolerates dither while never
# calling a real, quiet room dead.
SILENCE_PEAK: Final = 2


def mark_alive(source_dir: Path) -> None:
    """Refresh the source's liveness marker — call only on measured signal."""
    (source_dir / ALIVE_FILE).touch()


def alive_mtime(source_dir: Path) -> datetime | None:
    """When the source last proved it was recording, or None if never/unreadable."""
    try:
        mtime = (source_dir / ALIVE_FILE).stat().st_mtime
    except OSError:
        return None
    return datetime.fromtimestamp(mtime, tz=UTC)


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


def container_ext(codec: str) -> str:
    """File extension for segment files produced with `codec`."""
    return _CODEC_EXT.get(codec, "mka")


@dataclass(frozen=True)
class CaptureConfig:
    """Capture parameters.

    Defaults to Opus at 32 kbps — perceptually transparent for speech and ~11x
    smaller than lossless FLAC, with identical transcription. Lossless buys
    nothing for a speech memory aid (Whisper uses 16 kHz mono); the meaningful
    resolution — what was said, by whom, how it sounded — is fully preserved.
    """

    sample_rate: int = 48000
    channels: int = 1
    segment_seconds: int = 60
    codec: str = "libopus"
    bitrate: str | None = "32k"
    loglevel: str = "warning"


def build_segment_argv(
    config: CaptureConfig, output_pattern: str, *, fanout: bool = False
) -> list[str]:
    """ffmpeg argv that reads raw s16le PCM from stdin and writes segment files.

    The producer (recall.sources) supplies the PCM stream on this process's
    stdin; ffmpeg only segments and encodes — it never touches the device.

    `fanout` appends the best-effort UDP live tap as a second output (see
    recall.sources.fanout_output_argv), so recall-live never opens the device.
    """
    argv = [
        "ffmpeg",
        "-hide_banner",
        "-loglevel",
        config.loglevel,
        "-f",
        "s16le",
        "-ar",
        str(config.sample_rate),
        "-ac",
        str(config.channels),
        "-i",
        "-",
        "-c:a",
        config.codec,
    ]
    if config.bitrate is not None:
        argv += ["-b:a", config.bitrate]
    if config.codec == "libopus":
        argv += ["-application", "voip"]  # voice-optimised
    argv += [
        "-f",
        "segment",
        "-segment_time",
        str(config.segment_seconds),
        "-reset_timestamps",
        "1",
        "-strftime",
        "1",
        output_pattern,
    ]
    if fanout:
        argv += fanout_output_argv()
    return argv


def segment_output_pattern(directory: str, source_id: str, *, ext: str = "flac") -> str:
    """ffmpeg output pattern: `<directory>/<source_id>/<source_id>-<ts>.<ext>`."""
    return f"{directory}/{source_id}/{source_id}-{_TS_STRFTIME}.{ext}"


def parse_segment_start(filename: str) -> datetime:
    """Parse the UTC start time embedded in a segment filename."""
    match = _TS_RE.search(filename)
    if match is None:
        msg = f"no timestamp found in segment filename {filename!r}"
        raise ValueError(msg)
    naive = datetime.strptime(match.group(1), _TS_PARSE)
    return naive.replace(tzinfo=UTC)
