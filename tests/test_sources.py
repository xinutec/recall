"""The pluggable audio-source abstraction (ffmpeg mic + network/synthetic)."""

from __future__ import annotations

import pytest

from recall.sources import (
    DEVICE_KINDS,
    FANOUT_PORT,
    SWEEPABLE_KINDS,
    AudioSource,
    SourceKind,
    live_input_argv,
)


def test_live_input_reads_the_udp_tap() -> None:
    argv = live_input_argv()
    assert argv[0] == "ffmpeg"
    assert "avfoundation" not in argv  # NOT a device client
    assert argv[argv.index("-i") + 1].startswith(f"udp://127.0.0.1:{FANOUT_PORT}")
    assert argv[-3:] == ["-f", "s16le", "-"]  # PCM to stdout


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


def test_a_discovered_source_is_neither_a_device_nor_sweepable() -> None:
    # Both exclusions are the conservative reading of "we don't know what this is":
    # it must not be health-checked as a microphone that stopped, and it must not be
    # deletable as idle room noise — it may be an uploaded recording.
    assert SourceKind.DISCOVERED not in DEVICE_KINDS
    assert SourceKind.DISCOVERED not in SWEEPABLE_KINDS
    # the kinds that ARE recorders stay in both
    assert SourceKind.COREAUDIO in DEVICE_KINDS
    assert SourceKind.TCP_PCM in SWEEPABLE_KINDS


def test_blank_id_is_rejected() -> None:
    with pytest.raises(ValueError, match="id"):
        AudioSource(id="", name="x", kind=SourceKind.COREAUDIO, spec="")


def test_id_must_be_filesystem_safe() -> None:
    with pytest.raises(ValueError, match="safe"):
        AudioSource(id="us/b", name="x", kind=SourceKind.COREAUDIO, spec="")
