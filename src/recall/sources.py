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
    # Audio the worker found on disk with no registered source. It is an admission,
    # not a producer: nothing here says what wrote those files. The worker used to
    # answer COREAUDIO, which silently turned a meeting someone copied into the data
    # root into a microphone — invisible in the sessions list, health-checked as a mic
    # that had stopped, and sweepable. Whoever actually knows (the capture agent, the
    # ingest handshake, an upload) corrects it via `Store.register_source`.
    DISCOVERED = "discovered"


# Kinds the quiet review may delete and a fleet sweep may target: everything recall
# captured itself, never an uploaded recording (a meeting is real speech we chose to
# keep, not idle room noise). The single source of truth — quiet detection, the Mac's
# sweep-safety check, and the audio-volume query all read this one set.
SWEEPABLE_KINDS: frozenset[SourceKind] = frozenset(SourceKind) - {
    SourceKind.UPLOAD,
    # DISCOVERED means "we don't know what this is", and deletion is irreversible:
    # anything we can't positively identify as our own capture stays.
    SourceKind.DISCOVERED,
}

# Kinds with a live recorder behind them — a device that can stall, die, or be carried
# out of the house. An UPLOAD arrives over HTTP as a finished file, so there is no
# capture to go wrong: it is a source, but never a *device*. Every health question about
# recording ("is it live?", "did it lose speech?") applies to these and only these,
# which is why an imported meeting must not appear as a microphone that stopped.
#
# Deliberately its own set, not an alias of SWEEPABLE_KINDS: they coincide today but
# answer different questions (what may be deleted vs what is being recorded).
DEVICE_KINDS: frozenset[SourceKind] = frozenset(SourceKind) - {
    SourceKind.UPLOAD,
    # A DISCOVERED source has no recorder to be up or down, so every device check
    # would be answering a question about a machine that may not exist. If it really
    # is a recorder, its agent registers the true kind on start and it joins this set.
    SourceKind.DISCOVERED,
}


class SourceRow(NamedTuple):
    """A registered source as the fleet/liveness view needs it (Store.source_rows),
    with `kind` already parsed to the enum so callers never compare raw strings."""

    id: str
    name: str
    kind: SourceKind


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
