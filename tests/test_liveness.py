"""Per-source liveness: active iff its last-proved-recording time is fresh, with a
window matched to how each kind's marker is refreshed (per chunk for a streamed
phone, per 30s watchdog poll for the local mic)."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta

from recall.liveness import source_statuses

NOW = datetime(2026, 6, 22, 12, 0, 0, tzinfo=UTC)
SOURCES = [
    ("usb", "usb", "coreaudio"),
    ("pixel9", "Pixel 9", "tcp_pcm"),
    ("pixel5", "Pixel 5", "tcp_pcm"),
]


def test_fresh_is_active_stale_is_idle_missing_is_idle() -> None:
    last_active = {
        "usb": NOW - timedelta(seconds=2),  # streaming now
        "pixel9": NOW - timedelta(minutes=5),  # went idle
        # pixel5 absent → never seen
    }
    by_id = {s.source_id: s for s in source_statuses(SOURCES, last_active, NOW)}
    assert by_id["usb"].active is True
    assert by_id["pixel9"].active is False
    assert by_id["pixel5"].active is False
    assert by_id["pixel5"].last_active is None
    # name/kind carried through for the panel
    assert by_id["pixel9"].name == "Pixel 9"
    assert by_id["pixel9"].kind == "tcp_pcm"


def test_mic_window_matches_the_watchdog_cadence_not_the_stream_window() -> None:
    # The mic's marker is refreshed every ~30s watchdog poll, so a 40s-old marker is
    # a healthy mic — while a phone's per-chunk marker at 40s means it stopped.
    last_active = {
        "usb": NOW - timedelta(seconds=40),
        "pixel9": NOW - timedelta(seconds=40),
    }
    by_id = {s.source_id: s for s in source_statuses(SOURCES, last_active, NOW)}
    assert by_id["usb"].active is True
    assert by_id["pixel9"].active is False


def test_fleet_widens_every_window_by_the_report_lag() -> None:
    # On Isis the markers arrive via the Mac's ~5s mirror report: a phone marker 8s
    # old is within one report cadence of fresh (idle locally, active on the fleet).
    last_active = {
        "pixel9": NOW - timedelta(seconds=8),
        "usb": NOW - timedelta(seconds=78),
    }
    local = {s.source_id: s for s in source_statuses(SOURCES, last_active, NOW)}
    fleet = {
        s.source_id: s
        for s in source_statuses(SOURCES, last_active, NOW, on_fleet=True)
    }
    assert local["pixel9"].active is False
    assert fleet["pixel9"].active is True
    assert local["usb"].active is False
    assert fleet["usb"].active is True
