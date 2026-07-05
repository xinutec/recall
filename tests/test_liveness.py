"""Per-source liveness: active iff its last-activity is within the window."""

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


def test_window_boundary() -> None:
    win = timedelta(seconds=6)
    last_active = {
        "usb": NOW - timedelta(seconds=5),  # inside
        "pixel9": NOW - timedelta(seconds=7),  # outside
    }
    by_id = {
        s.source_id: s
        for s in source_statuses(SOURCES, last_active, NOW, active_within=win)
    }
    assert by_id["usb"].active is True
    assert by_id["pixel9"].active is False
