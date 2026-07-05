"""ffmpeg segmentation/encoding of a raw PCM stream, plus filename helpers.

Capture reads a continuous raw-PCM stream from a source (see recall.sources) and
pipes it into ffmpeg's `segment` muxer, which writes a continuous ring of
fixed-length files — a crash loses at most one segment. Filenames embed a UTC
start timestamp (ffmpeg is run with TZ=UTC).

Only pure construction/parsing lives here; running the pipe is in recall.runner.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from datetime import UTC, datetime
from typing import Final

# ffmpeg -strftime token used in segment filenames, e.g. usb-20260613T140530.flac
_TS_STRFTIME: Final = "%Y%m%dT%H%M%S"
_TS_PARSE: Final = "%Y%m%dT%H%M%S"
_TS_RE: Final = re.compile(r"(\d{8}T\d{6})")

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


def build_segment_argv(config: CaptureConfig, output_pattern: str) -> list[str]:
    """ffmpeg argv that reads raw s16le PCM from stdin and writes segment files.

    The producer (recall.sources) supplies the PCM stream on this process's
    stdin; ffmpeg only segments and encodes — it never touches the device.
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
