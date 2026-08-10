"""ffmpeg segmentation/encoding — pure command construction and filename parsing.

The live recording itself needs the mic (and macOS permission); everything here
is pure and testable without it.
"""

from __future__ import annotations

from datetime import UTC, datetime

import pytest

from recall.capture import (
    CaptureConfig,
    build_segment_argv,
    container_ext,
    parse_segment_start,
    segment_output_pattern,
)
from recall.sources import FANOUT_PORT


def test_default_config_is_opus_for_speech() -> None:
    cfg = CaptureConfig()
    # Opus, transparent for speech, ~11x smaller than FLAC
    assert cfg.codec == "libopus"
    assert cfg.bitrate == "32k"
    assert cfg.sample_rate == 48000
    assert cfg.segment_seconds == 60


def test_opus_argv_has_bitrate_and_voice_mode() -> None:
    argv = build_segment_argv(CaptureConfig(), "/data/usb/usb-%Y%m%dT%H%M%S.opus")
    assert argv[argv.index("-c:a") + 1] == "libopus"
    assert argv[argv.index("-b:a") + 1] == "32k"
    assert argv[argv.index("-application") + 1] == "voip"


def test_build_segment_argv_reads_pcm_from_stdin() -> None:
    cfg = CaptureConfig()
    pattern = "/data/usb/usb-%Y%m%dT%H%M%S.flac"
    argv = build_segment_argv(cfg, pattern)

    assert argv[0] == "ffmpeg"
    # input is raw s16le from stdin, not a device
    assert argv[argv.index("-f") : argv.index("-f") + 2] == ["-f", "s16le"]
    assert argv[argv.index("-i") + 1] == "-"
    # segmented output muxer
    assert "segment" in argv
    assert argv[argv.index("-segment_time") + 1] == "60"
    assert "-strftime" in argv
    assert argv[argv.index("-ar") + 1] == "48000"
    assert argv[-1] == pattern  # output pattern is last


def test_build_segment_argv_fanout_appends_the_live_tap() -> None:
    # The segmenter (not the sox producer, which has no second output) publishes the
    # best-effort 16 kHz UDP copy recall-live consumes. It comes AFTER the segment
    # output, and UDP is fire-and-forget, so it can't backpressure the archive.
    pattern = "/data/usb/usb-%Y%m%dT%H%M%S.opus"
    argv = build_segment_argv(CaptureConfig(), pattern, fanout=True)
    udp = next(i for i, a in enumerate(argv) if a.startswith("udp://"))
    assert udp > argv.index(pattern)  # tap is the SECOND output, after the segments
    assert f":{FANOUT_PORT}" in argv[udp]
    assert argv[udp - 6 : udp] == ["-ar", "16000", "-ac", "1", "-f", "s16le"]


def test_build_segment_argv_without_fanout_has_no_udp_output() -> None:
    argv = build_segment_argv(CaptureConfig(), "/data/usb/usb-%Y%m%dT%H%M%S.opus")
    assert not any(a.startswith("udp://") for a in argv)


def test_build_segment_argv_respects_overrides() -> None:
    cfg = CaptureConfig(
        sample_rate=16000,
        channels=2,
        segment_seconds=30,
        codec="pcm_s16le",
        bitrate=None,
    )
    argv = build_segment_argv(cfg, "/data/x-%Y%m%dT%H%M%S.wav")
    assert argv[argv.index("-ar") + 1] == "16000"
    assert argv[argv.index("-ac") + 1] == "2"
    assert argv[argv.index("-segment_time") + 1] == "30"
    assert argv[argv.index("-c:a") + 1] == "pcm_s16le"


def test_container_ext_maps_codec_to_extension() -> None:
    assert container_ext("flac") == "flac"
    assert container_ext("pcm_s16le") == "wav"
    assert container_ext("libopus") == "opus"


def test_segment_output_pattern() -> None:
    pattern = segment_output_pattern("/data", "usb", ext="flac")
    assert pattern == "/data/usb/usb-%Y%m%dT%H%M%S.flac"


def test_parse_segment_start_is_utc() -> None:
    dt = parse_segment_start("usb-20260613T140530.flac")
    assert dt == datetime(2026, 6, 13, 14, 5, 30, tzinfo=UTC)


def test_parse_segment_start_tolerates_dashed_source_id() -> None:
    dt = parse_segment_start("phone-lan-20260613T000000.flac")
    assert dt == datetime(2026, 6, 13, 0, 0, 0, tzinfo=UTC)


def test_parse_segment_start_rejects_garbage() -> None:
    with pytest.raises(ValueError, match="timestamp"):
        parse_segment_start("not-a-segment.flac")


def test_the_capture_consumer_still_takes_its_audio_on_stdin() -> None:
    """The counterpart to `-nostdin` on the derived-copy builders.

    Every other ffmpeg in recall is told to leave stdin alone; this one IS the
    stdin reader — sox owns the mic and hands the PCM over on it (`-i -`). A
    blanket sweep that added the flag here would stop capture recording, and
    nothing else in the suite would notice, so the guard is here.
    """
    argv = build_segment_argv(CaptureConfig(), "/tmp/usb-%Y%m%dT%H%M%S.opus")
    assert "-nostdin" not in argv
    assert argv[argv.index("-i") + 1] == "-"
