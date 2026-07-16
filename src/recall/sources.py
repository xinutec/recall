"""Pluggable audio-source abstraction.

Every source knows how to emit a continuous raw little-endian s16 PCM stream on
stdout, so capture orchestration is source-agnostic. The macOS mic uses **sox
(CoreAudio)** rather than ffmpeg's avfoundation input — the latter drops ~20% of
samples on this machine (re-measured 2026-07-16: 15 s wall → 11.6 s of audio,
same ratio at 30 s, thread_queue_size no help), which is unacceptable for a
"never lose a word" record. sox delivers sample-perfect audio but its CoreAudio
read can rarely wedge to digital zeros (the 2026-07-15 dead-window); the runner's
dead-segment watchdog cycles the producer when that happens, so the wedge costs
minutes, not a recording.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from enum import Enum
from typing import Final, NamedTuple

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

    COREAUDIO = "coreaudio"  # sox — sample-perfect macOS device capture
    LAVFI = "lavfi"  # ffmpeg synthetic source (testing/calibration)
    RTSP = "rtsp"  # ffmpeg network source (e.g. a phone on the LAN)
    TCP_PCM = "tcp_pcm"  # ffmpeg listens for raw s16le PCM (the recall-mic app)
    UPLOAD = "upload"  # clips uploaded over HTTP (e.g. phone recorder); not captured


class SourceRow(NamedTuple):
    """A registered source as the fleet/liveness view needs it (Store.source_rows),
    with `kind` already parsed to the enum so callers never compare raw strings."""

    id: str
    name: str
    kind: SourceKind


def _ffmpeg_pcm_tail(
    sample_rate: int, channels: int, max_seconds: int | None
) -> list[str]:
    tail = []
    if max_seconds is not None:
        tail += ["-t", str(max_seconds)]
    tail += ["-ar", str(sample_rate), "-ac", str(channels), "-f", "s16le", "-"]
    return tail


def fanout_output_argv() -> list[str]:
    """A second ffmpeg output: the best-effort live tap. The SEGMENTER appends this
    after its segment output, so its one PCM input feeds both — the archive (reliable)
    and the live feed (this UDP, droppable). Fire-and-forget, so it can't stall the
    archive. It lives on the segmenter (not the producer) because the producer is sox,
    which has no second output — and the segmenter sees the identical byte stream.
    Unbounded on purpose: the segmenter ends at the producer's EOF, which closes every
    output, so a bounded record still exits."""
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
    ) -> list[str]:
        """Command that streams raw s16le PCM for this source to stdout."""
        match self.kind:
            case SourceKind.COREAUDIO:
                # sox, NOT ffmpeg avfoundation: avfoundation continuously drops ~20%
                # of samples on this machine (words vanish mid-sentence); sox is
                # sample-perfect. sox's known failure — its CoreAudio read rarely
                # wedges to digital zeros — is covered by the runner's dead-segment
                # watchdog, which cycles the producer within minutes and records it.
                # An unknown device name makes sox fail hard (the launchd agent
                # crash-loops visibly) — never a silent fallback to the default,
                # which a Bluetooth handsfree mic can grab.
                device = ["-t", "coreaudio", self.spec] if self.spec else ["-d"]
                # -q: no progress meter — it repaints stderr several times a second,
                # burying the agent log's real lines. Errors still print.
                argv = [
                    "sox",
                    "-q",
                    *device,
                    "-c",
                    str(channels),
                    "-r",
                    str(sample_rate),
                    "-b",
                    "16",
                    "-t",
                    "raw",
                    "-e",
                    "signed-integer",
                    "-",
                ]
                if max_seconds is not None:
                    argv += ["trim", "0", str(max_seconds)]
                return argv
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
