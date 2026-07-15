"""Pluggable audio-source abstraction.

Every source knows how to emit a continuous raw little-endian s16 PCM stream on
stdout, so capture orchestration is source-agnostic. The macOS mic uses **sox
(CoreAudio)** rather than ffmpeg's avfoundation input — the latter drops ~20% of
samples on this machine, which is unacceptable for a "never lose a word" record.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from enum import Enum
from typing import Final

_SAFE_ID: re.Pattern[str] = re.compile(r"^[a-z0-9][a-z0-9-]*$")

# A network PCM stream is continuous, so no data for this long means the
# connection is dead — e.g. a half-open socket after a network blip left the peer's
# FIN unseen. ffmpeg then errors out and the launchd agent relistens, instead of
# wedging forever on a quietly-dead socket. 15s tolerates brief Wi-Fi stalls.
_TCP_READ_TIMEOUT_US: Final = 15_000_000

# The live-feed tap. Only ONE process may hold the CoreAudio device — two clients on one
# device starve each other (proven 2026-07-15: capture + live both got silence). So the
# single mic reader (capture) emits a SECOND, best-effort PCM copy on this localhost UDP
# port, and recall-live reads that, not the device. UDP is fire-and-forget:
# a full or absent receiver just drops packets, so the tap can NEVER backpressure the
# archive segmenter (completeness stays the segmenter's alone). 16 kHz mono = live's
# format, so live needs no resampling.
FANOUT_HOST: Final = "127.0.0.1"
FANOUT_PORT: Final = 9876
FANOUT_SAMPLE_RATE: Final = 16000
FANOUT_CHANNELS: Final = 1
# Datagram payload that fits a loopback packet without IP fragmentation.
_FANOUT_PKT_SIZE: Final = 1316


class SourceKind(Enum):
    """How a source's PCM stream is produced."""

    COREAUDIO = "coreaudio"  # ffmpeg (avfoundation) — reliable macOS device capture
    LAVFI = "lavfi"  # ffmpeg synthetic source (testing/calibration)
    RTSP = "rtsp"  # ffmpeg network source (e.g. a phone on the LAN)
    TCP_PCM = "tcp_pcm"  # ffmpeg listens for raw s16le PCM (the recall-mic app)
    UPLOAD = "upload"  # clips uploaded over HTTP (e.g. phone recorder); not captured


def _ffmpeg_pcm_tail(
    sample_rate: int, channels: int, max_seconds: int | None
) -> list[str]:
    tail = []
    if max_seconds is not None:
        tail += ["-t", str(max_seconds)]
    tail += ["-ar", str(sample_rate), "-ac", str(channels), "-f", "s16le", "-"]
    return tail


def _fanout_output() -> list[str]:
    """A second ffmpeg output: the best-effort live tap. Appended after the stdout PCM
    output so one mic input feeds both — the archive (stdout, reliable) and the live
    feed (this UDP, droppable). Fire-and-forget, so it can't stall the archive."""
    url = f"udp://{FANOUT_HOST}:{FANOUT_PORT}?pkt_size={_FANOUT_PKT_SIZE}"
    return [
        "-ar",
        str(FANOUT_SAMPLE_RATE),
        "-ac",
        str(FANOUT_CHANNELS),
        "-f",
        "s16le",
        url,
    ]


def live_input_argv() -> list[str]:
    """ffmpeg that reads the best-effort UDP tap capture publishes (FANOUT_*) and sends
    it to stdout — what recall-live consumes INSTEAD of opening the mic, so only capture
    holds the device. `overrun_nonfatal` + a fifo keep a burst from killing the reader;
    the tap is already 16 kHz mono, so this is a pass-through."""
    url = (
        f"udp://{FANOUT_HOST}:{FANOUT_PORT}"
        f"?overrun_nonfatal=1&fifo_size={_FANOUT_PKT_SIZE * 64}"
    )
    return [
        "ffmpeg",
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "s16le",
        "-ar",
        str(FANOUT_SAMPLE_RATE),
        "-ac",
        str(FANOUT_CHANNELS),
        "-i",
        url,
        "-f",
        "s16le",
        "-",
    ]


@dataclass(frozen=True)
class AudioSource:
    """A single audio input.

    `spec` is kind-specific: a CoreAudio device name for COREAUDIO (empty = the
    system default input — avoid: a Bluetooth speaker's hands-free mic can *become*
    the default and silently replace the real mic), an ffmpeg lavfi description for
    LAVFI, or a URL for RTSP. `id` becomes part of segment filenames and directory
    names, so it must be filesystem-safe.
    """

    id: str
    name: str
    kind: SourceKind
    spec: str

    def __post_init__(self) -> None:
        if not self.id:
            msg = "source id must not be empty"
            raise ValueError(msg)
        if not _SAFE_ID.match(self.id):
            msg = f"source id {self.id!r} is not filesystem-safe (use [a-z0-9-])"
            raise ValueError(msg)

    @property
    def port(self) -> int | None:
        """The TCP listen port for a TCP_PCM source (parsed from `spec`, e.g.
        "0.0.0.0:9899" -> 9899); None for sources that aren't a TCP listener."""
        if self.kind is not SourceKind.TCP_PCM or ":" not in self.spec:
            return None
        try:
            return int(self.spec.rsplit(":", 1)[1])
        except ValueError:
            return None

    def producer_argv(
        self,
        sample_rate: int,
        channels: int,
        *,
        max_seconds: int | None = None,
        fanout: bool = False,
    ) -> list[str]:
        """Command that streams raw s16le PCM for this source to stdout.

        `fanout` (COREAUDIO only) appends a second, best-effort UDP output: the live tap
        capture publishes so recall-live needn't open the device (see _fanout_output).
        It cannot backpressure the stdout/archive output, so completeness is safe."""
        match self.kind:
            case SourceKind.COREAUDIO:
                # ffmpeg avfoundation, NOT sox. sox's CoreAudio driver wedges: its input
                # silently drops to digital zero for minutes while the device stays
                # healthy — proven side-by-side (avfoundation read real audio from the
                # mic the instant sox was writing empty segments). That wedge is the
                # "dead-window" that lost the opening/middle of recordings. avfoundation
                # reads the device reliably; `-i ":<device>"` is audio-only. An unknown
                # name still fails hard (the agent crash-loops visibly) — never a silent
                # fallback to the default, which a Bluetooth handsfree mic can grab.
                device = self.spec if self.spec else "default"
                return [
                    "ffmpeg",
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-f",
                    "avfoundation",
                    "-i",
                    f":{device}",
                    *_ffmpeg_pcm_tail(sample_rate, channels, max_seconds),
                    *(_fanout_output() if fanout else []),
                ]
            case SourceKind.LAVFI:
                return [
                    "ffmpeg",
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-re",
                    "-f",
                    "lavfi",
                    "-i",
                    self.spec,
                    *_ffmpeg_pcm_tail(sample_rate, channels, max_seconds),
                ]
            case SourceKind.RTSP:
                return [
                    "ffmpeg",
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-rtsp_transport",
                    "tcp",
                    "-i",
                    self.spec,
                    *_ffmpeg_pcm_tail(sample_rate, channels, max_seconds),
                ]
            case SourceKind.TCP_PCM:
                # The recall-mic phone app connects in and streams headerless
                # s16le PCM, so the input format must be declared before -i. ffmpeg
                # is the listener (the phone is the client); on disconnect ffmpeg
                # exits and the launchd agent relaunches it to listen again. The
                # read timeout makes that hold for a *half-open* drop too (a network
                # blip where the FIN never arrives), which would otherwise wedge the
                # listener — alive but no longer accepting — until killed by hand.
                #
                # The downstream segmenter names files by wallclock second
                # (-strftime), so it relies on audio arriving ~realtime: if a burst
                # were consumed faster than one segment per second, segments would
                # collide on the same filename and overwrite. Safe here because the
                # phone streams live mic (realtime) into >=60 s segments, and a
                # reconnect carries no backlog. Keep segment_seconds well above 1 s
                # for any network source.
                return [
                    "ffmpeg",
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-f",
                    "s16le",
                    "-ar",
                    str(sample_rate),
                    "-ac",
                    str(channels),
                    "-i",
                    f"tcp://{self.spec}?listen=1&timeout={_TCP_READ_TIMEOUT_US}",
                    *_ffmpeg_pcm_tail(sample_rate, channels, max_seconds),
                ]
            case SourceKind.UPLOAD:
                msg = "uploaded sources are not captured by recall"
                raise NotImplementedError(msg)
