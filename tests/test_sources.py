"""The pluggable audio-source abstraction (ffmpeg mic + network/synthetic)."""

from __future__ import annotations

import pytest

from recall.sources import (
    FANOUT_PORT,
    AudioSource,
    SourceKind,
    live_input_argv,
)


def test_coreaudio_uses_sox_default_device() -> None:
    # sox, not ffmpeg avfoundation: avfoundation continuously drops ~20% of samples
    # on this machine (measured 2026-07-16) — words vanish mid-sentence. sox is
    # sample-perfect; its rare CoreAudio wedge is the dead-segment watchdog's job.
    src = AudioSource(id="usb", name="USB", kind=SourceKind.COREAUDIO, spec="")
    argv = src.producer_argv(48000, 1)
    assert argv[0] == "sox"
    assert argv[1] == "-q"  # progress meter off - it floods the agent log
    assert argv[2] == "-d"  # the default device, only when no spec pins one
    assert argv[argv.index("-r") + 1] == "48000"
    assert argv[argv.index("-c") + 1] == "1"
    assert argv[argv.index("-e") + 1] == "signed-integer"
    assert "-" in argv  # raw PCM to stdout


def test_coreaudio_named_device_pins_the_sox_input() -> None:
    # A Bluetooth speaker with a hands-free mic (the Bose) can become the system default
    # input, so an unpinned default would record telephone-quality through it. A named
    # spec selects the exact CoreAudio device; an unknown name fails hard (crash-loops
    # visibly), never a silent fallback to the default.
    src = AudioSource(
        id="usb", name="USB", kind=SourceKind.COREAUDIO, spec="USB Condenser Microphone"
    )
    argv = src.producer_argv(48000, 1)
    assert argv[:4] == ["sox", "-q", "-t", "coreaudio"]
    assert argv[4] == "USB Condenser Microphone"
    assert "-d" not in argv


def test_coreaudio_bounded_limits_duration() -> None:
    src = AudioSource(id="usb", name="USB", kind=SourceKind.COREAUDIO, spec="")
    argv = src.producer_argv(48000, 1, max_seconds=10)
    assert argv[-3:] == ["trim", "0", "10"]  # sox duration cap


def test_coreaudio_producer_has_no_udp_output() -> None:
    # The live tap moved to the SEGMENTER (capture.build_segment_argv fanout): sox has
    # no second output, and the segmenter sees the identical byte stream anyway.
    src = AudioSource(id="usb", name="USB", kind=SourceKind.COREAUDIO, spec="")
    argv = src.producer_argv(48000, 1)
    assert not any(a.startswith("udp://") for a in argv)


def test_live_input_reads_the_udp_tap() -> None:
    argv = live_input_argv()
    assert argv[0] == "ffmpeg"
    assert "avfoundation" not in argv  # NOT a device client
    assert argv[argv.index("-i") + 1].startswith(f"udp://127.0.0.1:{FANOUT_PORT}")
    assert argv[-3:] == ["-f", "s16le", "-"]  # PCM to stdout


def test_lavfi_uses_ffmpeg_realtime_synthetic() -> None:
    src = AudioSource(
        id="synth", name="synth", kind=SourceKind.LAVFI, spec="sine=frequency=440"
    )
    argv = src.producer_argv(48000, 1, max_seconds=4)
    assert argv[0] == "ffmpeg"
    assert "-re" in argv  # paced to realtime so wallclock filenames differ
    assert argv[argv.index("-i") + 1] == "sine=frequency=440"
    assert argv[argv.index("-t") + 1] == "4"
    assert argv[-3:] == ["-f", "s16le", "-"]


def test_rtsp_uses_ffmpeg_tcp() -> None:
    src = AudioSource(
        id="phone", name="Phone", kind=SourceKind.RTSP, spec="rtsp://10.0.0.5/mic"
    )
    argv = src.producer_argv(48000, 2)
    assert argv[0] == "ffmpeg"
    assert argv[argv.index("-rtsp_transport") + 1] == "tcp"
    assert argv[argv.index("-i") + 1] == "rtsp://10.0.0.5/mic"
    assert argv[argv.index("-ac") + 1] == "2"


def test_tcp_pcm_listens_for_raw_pcm() -> None:
    src = AudioSource(
        id="phone", name="Phone", kind=SourceKind.TCP_PCM, spec="0.0.0.0:9899"
    )
    argv = src.producer_argv(48000, 1)
    assert argv[0] == "ffmpeg"
    i = argv.index("-i")
    # raw PCM carries no header, so the input format must be declared before -i.
    url = argv[i + 1]
    assert url.startswith("tcp://0.0.0.0:9899?")
    assert "listen=1" in url
    # a read timeout so a dead/half-open connection is detected, not wedged on.
    assert "timeout=" in url
    assert "s16le" in argv[:i]
    assert argv[:i].count("-ar") == 1  # input rate declared once, before -i
    assert argv[argv.index("-ar") + 1] == "48000"
    assert argv[-3:] == ["-f", "s16le", "-"]  # raw PCM to stdout


def test_tcp_pcm_bounded_appends_duration() -> None:
    src = AudioSource(
        id="phone", name="Phone", kind=SourceKind.TCP_PCM, spec="0.0.0.0:9899"
    )
    argv = src.producer_argv(48000, 1, max_seconds=5)
    assert argv[argv.index("-t") + 1] == "5"


def test_tcp_pcm_port_is_parsed_from_spec() -> None:
    # The listen port is the phone's identity; a heartbeat maps back to the source by
    # it, so a phone needs no manual device id.
    src = AudioSource(
        id="phone", name="Phone", kind=SourceKind.TCP_PCM, spec="0.0.0.0:9899"
    )
    assert src.port == 9899


def test_local_source_has_no_port() -> None:
    src = AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    assert src.port is None


def test_upload_source_is_not_captured() -> None:
    src = AudioSource(id="phone", name="Phone", kind=SourceKind.UPLOAD, spec="")
    with pytest.raises(NotImplementedError, match="not captured"):
        src.producer_argv(48000, 1)


def test_blank_id_is_rejected() -> None:
    with pytest.raises(ValueError, match="id"):
        AudioSource(id="", name="x", kind=SourceKind.COREAUDIO, spec="")


def test_id_must_be_filesystem_safe() -> None:
    with pytest.raises(ValueError, match="safe"):
        AudioSource(id="us/b", name="x", kind=SourceKind.COREAUDIO, spec="")
