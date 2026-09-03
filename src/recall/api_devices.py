"""The device-status HTTP surface: source liveness, heartbeats, outboxes.

Slice 2 of api.py's decomposition (#1342), same register pattern as
api_quiet/sync: dependencies are passed in — `data_root` as a getter because
the tests monkeypatch `api.DATA_ROOT`, and `fleet_capture_state` injected so
this module stays independent of the capture family it would otherwise import.
"""

from __future__ import annotations

from collections.abc import Callable
from datetime import UTC, datetime
from pathlib import Path

from fastapi import FastAPI

from recall import capture_control
from recall.api_models import HeartbeatIn, OutboxIn
from recall.capture import alive_mtime
from recall.liveness import source_statuses
from recall.mic_alive import Beat, read_beats, record_beat
from recall.outbox import OutboxReport, read_reports, record_report
from recall.schemas import (
    CaptureOut,
    HeartbeatOut,
    HeartbeatsOut,
    OkOut,
    OutboxesOut,
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
    """Mount /api/sources + /api/devices/*. `fleet_capture_state` is api.py's —
    injected rather than imported, so the capture family can move independently."""
    _register_sources_route(app, store_factory, data_root, fleet_capture_state)
    _register_device_report_routes(app, store_factory)


def _register_sources_route(
    app: FastAPI,
    store_factory: Callable[[], Store],
    data_root: Callable[[], Path],
    fleet_capture_state: Callable[[Store, datetime], CaptureOut],
) -> None:
    def _local_last_active(
        rows: list[SourceRow], now: datetime
    ) -> dict[str, datetime | None]:
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
        return last_active

    def _fleet_last_active(
        store: Store, rows: list[SourceRow], now: datetime
    ) -> dict[str, datetime | None]:
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
        return last_active

    @app.get("/api/sources")
    def sources() -> SourcesOut:
        """Per-recorder liveness for the fleet view: active iff the source's liveness
        marker — refreshed only on measured audio — is fresh, so a dot means recording,
        not connected. On the fleet (Isis), which runs no capture, the markers
        arrive via
        the Mac's ~5s mirror report; the windows widen to absorb the cadence
        (recall.liveness.active_window). Uploaded recordings (meetings) are sources but
        not live devices, so they're excluded — they live in the Sessions view."""
        now = datetime.now(UTC)
        on_fleet = capture_control.is_fleet()
        store = store_factory()
        try:
            rows = [r for r in store.source_rows() if r.kind in DEVICE_KINDS]
            last_active = (
                _fleet_last_active(store, rows, now)
                if on_fleet
                else _local_last_active(rows, now)
            )
        finally:
            store.close()
        statuses = source_statuses(rows, last_active, now, on_fleet=on_fleet)
        return {
            "items": [
                {
                    "id": s.source_id,
                    "name": s.name,
                    "kind": s.kind.value,
                    "active": s.active,
                    "lastActive": s.last_active.isoformat() if s.last_active else None,
                }
                for s in statuses
            ]
        }


def _register_device_report_routes(
    app: FastAPI, store_factory: Callable[[], Store]
) -> None:
    @app.post("/api/devices/outbox")
    def report_outbox(body: OutboxIn) -> OkOut:
        """A phone says what it is still holding.

        The gap this closes: an approved recording the phone cannot deliver was state
        no fleet component could see. The meeting recorder 401ed from the day it was
        written, retried out to a +1h23m backoff, and said "N recordings waiting to
        upload" throughout — the same sentence it shows when you are simply not home
        yet (#77). The Mac's doctor posts to fleetwatch every five minutes and had no
        way to know.

        Best-effort status, never control: an unparseable time is dropped rather than
        refused, because a phone on an older build should cost its own line and
        nothing else.
        """
        now = datetime.now(UTC)
        store = store_factory()
        try:
            record_report(
                store,
                OutboxReport(
                    device=body.device,
                    queued=max(0, body.queued),
                    oldest_queued_at=_iso_or_none(body.oldestQueuedAt),
                    failing=max(0, body.failing),
                    reason=body.reason,
                    at=now,
                ),
            )
        finally:
            store.close()
        return {"ok": True}

    @app.post("/api/devices/heartbeat")
    def record_heartbeat(body: HeartbeatIn) -> OkOut:
        """A mic app says it is still running (#837).

        The gap this closes: recall could not tell a dead recorder from a quiet room.
        The liveness marker is refreshed only by audio above the silence floor — right
        for "is it recording", useless for "is it alive" — and while capture is paused
        the ingest listener is closed entirely, so nothing streams and nothing is known.
        Capture was paused for the four days before this was written.

        ⚠ `at` is the SERVER's clock, not the phone's. A beat is evidence that this app
        reached the fleet just now, and a phone with a wrong clock would otherwise be
        able to report itself permanently fresh or permanently stale. The phone's own
        times are kept only where they say something about the phone (`startedAt`).

        Best-effort status, never control: an unparseable `startedAt` is dropped rather
        than refused, because an app on an older build should cost its own detail and
        nothing else — least of all the beat itself, which is the part that matters.
        """
        now = datetime.now(UTC)
        store = store_factory()
        try:
            record_beat(
                store,
                Beat(
                    device=body.device,
                    app=body.app,
                    version=body.version,
                    started_at=_iso_or_none(body.startedAt),
                    streaming=body.streaming,
                    charging=body.charging,
                    mic_ok=body.micOk,
                    via_lan=body.viaLan,
                    at=now,
                ),
            )
        finally:
            store.close()
        return {"ok": True}

    @app.get("/api/devices/heartbeat")
    def heartbeats() -> HeartbeatsOut:
        """Every mic app's last beat, for the fleetwatch collector that grades it.

        No verdict here, for the same reason as the outbox: what counts as too long
        belongs beside the other fleetwatch thresholds rather than split across two
        repositories.
        """
        store = store_factory()
        try:
            beats = read_beats(store)
        finally:
            store.close()
        return {"items": [_heartbeat(b) for b in beats]}

    def _heartbeat(beat: Beat) -> HeartbeatOut:
        return {
            "device": beat.device,
            "app": beat.app,
            "version": beat.version,
            "startedAt": beat.started_at.isoformat() if beat.started_at else None,
            "streaming": beat.streaming,
            "charging": beat.charging,
            "micOk": beat.mic_ok,
            "viaLan": beat.via_lan,
            "at": beat.at.isoformat(),
        }

    @app.get("/api/devices/outbox")
    def outboxes() -> OutboxesOut:
        """Every phone's last outbox report, for the fleetwatch collector that
        grades it.

        No verdict here on purpose. What counts as too long belongs with the other
        fleetwatch thresholds, beside the checks it will sit next to, rather than
        split across two repositories.
        """
        store = store_factory()
        try:
            reports = read_reports(store)
        finally:
            store.close()
        return {
            "items": [
                {
                    "device": r.device,
                    "queued": r.queued,
                    "oldestQueuedAt": (
                        r.oldest_queued_at.isoformat() if r.oldest_queued_at else None
                    ),
                    "failing": r.failing,
                    "reason": r.reason,
                    "at": r.at.isoformat(),
                }
                for r in reports
            ]
        }

    def _iso_or_none(value: str | None) -> datetime | None:
        if not value:
            return None
        try:
            return datetime.fromisoformat(value).astimezone(UTC)
        except ValueError:
            return None
