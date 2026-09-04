"""The Linux mic client: spool discipline, wire format, and the capture argv."""

from __future__ import annotations

import json

from recall.mic import ConnectionNarrator, PcmSpool, capture_argv, handshake_line
from recall.stream_server import parse_handshake


class TestPcmSpool:
    """Bounded, drop-oldest, counted — the capture thread must never block."""

    def test_holds_what_fits_and_drains_in_order(self) -> None:
        spool = PcmSpool(capacity_bytes=100)
        spool.offer(b"aaa")
        spool.offer(b"bbb")
        assert spool.drain() == b"aaabbb"
        assert spool.dropped == 0

    def test_drain_empties(self) -> None:
        spool = PcmSpool(capacity_bytes=100)
        spool.offer(b"xy")
        spool.drain()
        assert spool.drain() == b""

    def test_over_capacity_drops_the_oldest(self) -> None:
        spool = PcmSpool(capacity_bytes=6)
        spool.offer(b"aaa")
        spool.offer(b"bbb")
        spool.offer(b"ccc")  # pushes "aaa" out
        assert spool.drain() == b"bbbccc"

    def test_drops_are_counted_not_silent(self) -> None:
        """Audio lost here is invisible everywhere else, so the count is the only
        evidence it happened."""
        spool = PcmSpool(capacity_bytes=4)
        spool.offer(b"aaaa")
        assert spool.dropped == 0
        spool.offer(b"bb")
        assert spool.dropped == 2

    def test_a_chunk_larger_than_the_spool_keeps_its_tail(self) -> None:
        """Never store more than capacity, and never drop the whole chunk: the
        newest audio is the audio worth keeping."""
        spool = PcmSpool(capacity_bytes=4)
        spool.offer(b"abcdef")
        assert spool.drain() == b"cdef"
        assert spool.dropped == 2


class TestHandshakeLine:
    """The server parses this; a drift here is a source that never connects."""

    def test_round_trips_through_the_servers_own_parser(self) -> None:
        line = handshake_line("geb", rate=48000, channels=1, epoch=1756900000.25)
        shake = parse_handshake(line.decode().strip())
        assert shake is not None
        assert shake.source_id == "geb"
        assert shake.sample_rate == 48000
        assert shake.channels == 1
        assert shake.epoch == 1756900000.25

    def test_is_one_newline_terminated_line(self) -> None:
        line = handshake_line("geb", rate=48000, channels=1, epoch=1.0)
        assert line.endswith(b"\n")
        assert line.count(b"\n") == 1

    def test_epoch_is_fixed_point_never_exponent(self) -> None:
        """`float()` on the server accepts exponent form, but the phones both emit
        fixed point and a shared wire format is worth more than the tolerance."""
        line = handshake_line("geb", rate=48000, channels=1, epoch=1e12)
        assert b"e+" not in line and b"E+" not in line
        assert json.loads(line)["epoch"] == 1e12


class TestCaptureArgv:
    """ffmpeg owns the audio path; this is the only place its shape is written."""

    def test_opens_the_named_device_at_the_declared_input_shape(self) -> None:
        argv = capture_argv("hw:1,0", input_rate=48000, input_channels=2)
        assert "alsa" in argv
        assert "hw:1,0" in argv
        i = argv.index("-i")
        assert argv[i + 1] == "hw:1,0"
        # Input shape is stated, not inferred: a device that changes its format
        # should fail loudly rather than be silently resampled.
        assert argv[:i].count("-ac") == 1
        assert argv[:i].count("-ar") == 1

    def test_downmixes_to_mono_s16le_on_stdout(self) -> None:
        argv = capture_argv("hw:1,0", input_rate=48000, input_channels=2)
        i = argv.index("-i")
        assert argv[i + 2 :].count("-ac") == 1
        assert argv[argv.index("-ac", i) + 1] == "1"
        assert argv[-1] == "-"
        assert "s16le" in argv


class TestUnreachableLogging:
    """A paused household leaves the ingest listener closed for DAYS. The client is
    right to keep retrying every 2 s; narrating each attempt would write ~43,000 log
    lines a day and bury the reconnection somebody is actually looking for."""

    def test_repeated_failures_log_once_until_the_state_changes(self) -> None:
        narrator = ConnectionNarrator()
        assert narrator.should_report_failure() is True
        assert narrator.should_report_failure() is False
        assert narrator.should_report_failure() is False

    def test_a_success_rearms_the_next_failure_report(self) -> None:
        """The gap between "went quiet" and "came back" is the diagnostic, so each
        transition has to survive."""
        narrator = ConnectionNarrator()
        narrator.should_report_failure()
        narrator.note_connected()
        assert narrator.should_report_failure() is True
