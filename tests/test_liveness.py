"""Per-source liveness: active iff its last-proved-recording time is fresh, with a
window matched to how each kind's marker is refreshed (per chunk for a streamed
phone, per 30s watchdog poll for the local mic)."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta

from recall.liveness import source_statuses
from recall.sources import SourceKind, SourceRow

NOW = datetime(2026, 6, 22, 12, 0, 0, tzinfo=UTC)
SOURCES = [
    SourceRow("usb", "usb", SourceKind.COREAUDIO),
    SourceRow("pixel9", "Pixel 9", SourceKind.TCP_PCM),
    SourceRow("pixel5", "Pixel 5", SourceKind.TCP_PCM),
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
    assert by_id["pixel9"].kind is SourceKind.TCP_PCM


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


# --- store-and-forward recorders (#1428) -------------------------------------
#
# geb cut over to store-and-forward and vanished from the panel: it streams to
# nothing, so the .alive marker that "active" was built on is never refreshed
# again. Such a recorder proves itself by DELIVERING a closed segment instead.

GEB = SourceRow("geb", "geb", SourceKind.TCP_PCM)


def test_delivery_proves_recording_when_the_stream_marker_never_refreshes() -> None:
    # geb's marker froze the moment it stopped streaming, hours ago.
    last_active = {"geb": NOW - timedelta(hours=2)}
    # ...but a segment it captured a minute ago has landed.
    delivered = {"geb": NOW - timedelta(minutes=1)}
    status = source_statuses([GEB], last_active, NOW, delivered=delivered)[0]
    assert status.active is True
    # The panel must show the evidence that proved it, not the frozen marker.
    assert status.last_active == NOW - timedelta(minutes=1)


def test_a_recorder_that_stopped_delivering_goes_idle() -> None:
    delivered = {"geb": NOW - timedelta(minutes=30)}
    status = source_statuses([GEB], {}, NOW, delivered=delivered)[0]
    assert status.active is False
    assert status.last_active == NOW - timedelta(minutes=30)


def test_the_delivery_window_absorbs_segment_close_plus_upload_timer() -> None:
    # 60s of audio must close, then wait up to the 60s upload timer, then
    # transfer — so a two-minute-old capture is a HEALTHY store-and-forward
    # recorder, while the 5s stream window would call it dead.
    delivered = {"geb": NOW - timedelta(minutes=2)}
    assert source_statuses([GEB], {}, NOW, delivered=delivered)[0].active is True


def test_a_streaming_phone_keeps_its_own_fresher_evidence() -> None:
    # Phones stream AND shadow-deliver; the fresher signal wins, so the shadow
    # never drags a live phone's timestamp backwards.
    last_active = {"pixel9": NOW - timedelta(seconds=2)}
    delivered = {"pixel9": NOW - timedelta(minutes=2)}
    by_id = {
        s.source_id: s
        for s in source_statuses(SOURCES, last_active, NOW, delivered=delivered)
    }
    assert by_id["pixel9"].active is True
    assert by_id["pixel9"].last_active == NOW - timedelta(seconds=2)


def test_delivered_is_optional_so_the_mac_path_is_unchanged() -> None:
    last_active = {"usb": NOW - timedelta(seconds=2)}
    assert source_statuses(SOURCES, last_active, NOW)[0].active is True
