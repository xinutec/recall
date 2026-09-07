"""The device-status HTTP surface: source liveness, heartbeats, outboxes.

Slice 2 of api.py's decomposition (#1342), same register pattern as
api_work/sync: dependencies are passed in — `data_root` as a getter because
the tests monkeypatch `api.DATA_ROOT`, and `fleet_capture_state` injected so
this module stays independent of the capture family it would otherwise import.
"""

from __future__ import annotations

from collections.abc import Callable
from datetime import UTC, datetime
from pathlib import Path

from fastapi import FastAPI

from recall import capture_control
from recall.capture import alive_mtime
from recall.ingest_liveness import delivered_liveness
from recall.liveness import Evidence, source_statuses
from recall.schemas import (
    CaptureOut,
    SourcesOut,
)
from recall.sources import DEVICE_KINDS, SourceKind, SourceRow
from recall.store import Store


def register_device_routes(
    app: FastAPI,
    *,
    store_factory: Callable[[], Store],
    data_root: Callable[[], Path],
    fleet_capture_state: Callable[[Store, datetime], CaptureOut],
) -> None:
    """Mount /api/sources. `fleet_capture_state` is api.py's — injected rather than
    imported, so the capture family can move independently.

    ⚠ /api/devices/heartbeat and /api/devices/outbox are recalld's now. This route
    did NOT move with them: it reads the two-mode liveness model (Mac-local vs
    fleet) and takes `fleet_capture_state`, so it moves with the capture family or
    not at all."""
    _register_sources_route(app, store_factory, data_root, fleet_capture_state)


def _register_sources_route(
    app: FastAPI,
    store_factory: Callable[[], Store],
    data_root: Callable[[], Path],
    fleet_capture_state: Callable[[Store, datetime], CaptureOut],
) -> None:
    def _gate_delivered(
        delivered: dict[str, Evidence],
        rows: list[SourceRow],
        capture_running: bool,
    ) -> dict[str, Evidence]:
        """Delivered segments prove a recorder is running when nothing streams —
        but they are up to a segment old, so a PAUSE must discard them outright.
        Otherwise audio captured in the seconds before the pause keeps a dot
        green for the whole delivered window, which is the opposite of the
        promise a pause makes. This applies to EVERY kind, not just the local
        mic: a pause stops the phones and the machines too, so none of them may
        be resurrected by what they recorded just before it. Sources that are
        not devices (room, meetings) never reach this."""
        by_id = {row.id: row for row in rows}
        if not capture_running:
            return {}
        return {source: when for source, when in delivered.items() if source in by_id}

    def _local_last_active(
        rows: list[SourceRow], now: datetime, delivered: dict[str, Evidence]
    ) -> tuple[dict[str, datetime | None], dict[str, Evidence]]:
        """Liveness on the capturing host (the Mac), from each source's .alive marker —
        refreshed by the ingest pump while a phone streams real signal, and by the
        capture watchdog while the mic's closed segments decode to real audio
        (recall.capture.ALIVE_FILE) — so "active" means measured recording. The mic's
        marker is additionally gated on the pause state: its window is a leisurely
        ~75s (watchdog cadence), and a pause must read idle at once."""
        usb_recording = (
            capture_control.capture_running()
            and not capture_control.is_paused(data_root(), now)
        )
        last_active: dict[str, datetime | None] = {}
        for row in rows:
            marker = alive_mtime(data_root() / row.id)
            if row.kind is SourceKind.TCP_PCM:
                last_active[row.id] = marker
            else:
                last_active[row.id] = marker if usb_recording else None
        return last_active, _gate_delivered(delivered, rows, usb_recording)

    def _fleet_last_active(
        store: Store,
        rows: list[SourceRow],
        now: datetime,
        delivered: dict[str, Evidence],
    ) -> tuple[dict[str, datetime | None], dict[str, Evidence]]:
        """Liveness on the fleet (Isis), which runs no capture or ingest and cannot see
        the Mac's markers. It comes entirely from the Mac's mirror report
        (recall.capture_mirror), which ships every source's .alive freshness — the same
        measured-recording signal the Mac serves locally, one report cadence older. The
        mic keeps its local pause gate. A quiet Mac (no fresh report) reads as no one
        live — correct: the fleet genuinely does not know, and fleetwatch covers a dead
        Mac separately."""
        usb_recording = fleet_capture_state(store, now)["running"]
        reported = capture_control.reported_source_liveness(store, now) or {}
        last_active: dict[str, datetime | None] = {}
        for row in rows:
            if row.kind is SourceKind.TCP_PCM:
                last_active[row.id] = reported.get(row.id)
            else:
                last_active[row.id] = reported.get(row.id) if usb_recording else None
        return last_active, _gate_delivered(delivered, rows, usb_recording)

    @app.get("/api/sources")
    def sources() -> SourcesOut:
        """Per-recorder liveness for the fleet view, from TWO kinds of proof.

        A streaming recorder proves itself by its liveness marker — refreshed only
        on measured audio, so a dot means recording, not connected. On the fleet
        (Isis), which runs no capture, the markers arrive via the Mac's ~5s mirror
        report; the windows widen to absorb the cadence
        (recall.liveness.active_window).

        A STORE-AND-FORWARD recorder streams to nothing, so no marker of its is
        ever refreshed and the marker view alone reports it dead while it records
        perfectly (#1428 — geb, after the C3 cutover). It proves itself by
        DELIVERING a closed segment instead (recall.ingest_liveness), on its own
        wider window. The mic keeps its pause gate against both, so a pause still
        reads idle at once.

        Uploaded recordings (meetings) are sources but not live devices, so they're
        excluded — they live in the Sessions view."""
        now = datetime.now(UTC)
        on_fleet = capture_control.is_fleet()
        # Best-effort second signal; {} when recalld cannot answer, which leaves
        # the marker view exactly as it was (recall.ingest_liveness).
        delivered = delivered_liveness()
        store = store_factory()
        try:
            rows = [r for r in store.source_rows() if r.kind in DEVICE_KINDS]
            last_active, shipped = (
                _fleet_last_active(store, rows, now, delivered)
                if on_fleet
                else _local_last_active(rows, now, delivered)
            )
        finally:
            store.close()
        statuses = source_statuses(
            rows, last_active, now, on_fleet=on_fleet, delivered=shipped
        )
        return {
            "items": [
                {
                    "id": s.source_id,
                    "name": s.name,
                    "kind": s.kind.value,
                    "active": s.active,
                    "lastActive": s.last_active.isoformat() if s.last_active else None,
                    # Separate from `active` on purpose: `active` is the consent
                    # signal and goes out in a silent room, `recording` answers
                    # "is this thing on" (#1428).
                    "recording": s.recording,
                    "lastDelivered": (
                        s.last_delivered.isoformat() if s.last_delivered else None
                    ),
                }
                for s in statuses
            ]
        }
