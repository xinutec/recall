"""The capture-control HTTP surface: status (long-poll), pause, resume.

Slice 4 of api.py's decomposition (#1342) — the intricate one: the settled/
transitioning state machine, the stateToken long-poll, and the pause-origin
audit (#1347). Handlers stay MODULE-LEVEL (unlike the other slices' closures)
because the capture tests exercise them directly with hand-built Requests; the
two dependencies are module state set once by the registrar, patchable the
same way api.DATA_ROOT always was.
"""

from __future__ import annotations

import hashlib
import json
import logging
import time
from collections.abc import Callable
from datetime import UTC, datetime
from pathlib import Path

from fastapi import FastAPI, Request

from recall import capture_control
from recall.schemas import CaptureOut
from recall.store import Store
from recall.webauth import (
    COOKIE_NAME,
    WebAuthConfig,
    request_origin,
)

_log = logging.getLogger("recall.api")

# Set by register_capture_routes; reading them before registration is a
# programming error and fails loudly.
_store_factory: Callable[[], Store] | None = None
_data_root: Callable[[], Path] | None = None


def _store() -> Store:
    assert _store_factory is not None, "register_capture_routes was never called"
    return _store_factory()


def _root() -> Path:
    assert _data_root is not None, "register_capture_routes was never called"
    return _data_root()


def register_capture_routes(
    app: FastAPI,
    *,
    store_factory: Callable[[], Store],
    data_root: Callable[[], Path],
) -> None:
    """Mount /api/capture*. Dependencies land in module state (see header)."""
    global _store_factory, _data_root  # noqa: PLW0603 - the registrar's one job
    _store_factory = store_factory
    _data_root = data_root
    app.get("/api/capture")(capture_status)
    app.post("/api/capture/pause")(capture_pause)
    app.post("/api/capture/resume")(capture_resume)


def _with_token(state: CaptureOut) -> CaptureOut:
    """Stamp the state's fingerprint (CaptureOut.stateToken): the value a long-poll
    echoes back as ?known= so "unchanged" is the server's judgement, not the
    client's field-by-field comparison."""
    payload = {k: v for k, v in state.items() if k != "stateToken"}
    digest = hashlib.sha256(json.dumps(payload, sort_keys=True).encode())
    state["stateToken"] = digest.hexdigest()[:12]
    return state


def _capture_state(running: bool) -> CaptureOut:
    """Local (capturing-host) view: the pause file IS the actuation, so desired and
    confirmed are the same thing and the state is settled by construction."""
    until = capture_control.paused_until(_root())
    iso = until.isoformat() if until else None
    return _with_token(
        {
            "running": running,
            "pausedUntil": iso,
            "desiredRunning": running,
            "desiredPausedUntil": iso,
            "settled": True,
            "micReachable": True,
            "stateToken": "",
        }
    )


def fleet_capture_state(store: Store, now: datetime) -> CaptureOut:
    """The fleet holds the *desired* state (intent) while the Mac actuates and
    reports back, so the two can disagree for a couple of mirror cycles. Serve both:
    running/pausedUntil carry the mic's confirmed word (falling back to desired when
    it isn't reporting), desired* carries the intent, and settled says whether they
    agree — the client renders the disagreement as "Pausing…"/"Resuming…" instead of
    flapping between two truths it can't tell apart."""
    until = capture_control.intent_until(store, now)
    desired_running = until is None
    desired_until = until.isoformat() if until else None
    reported = capture_control.reported_state(store, now)
    if reported is None:
        return _with_token(
            {
                "running": desired_running,
                "pausedUntil": desired_until,
                "desiredRunning": desired_running,
                "desiredPausedUntil": desired_until,
                "settled": False,
                "micReachable": False,
                "stateToken": "",
            }
        )
    # Settled = the mic confirmed the desired state. When paused, the resume-by must
    # match too, so extending a pause (snooze) reads as transitioning until applied;
    # the Mac round-trips the intent's exact ISO string, so equality is exact.
    settled = reported.running == desired_running and (
        desired_running or reported.paused_until == desired_until
    )
    return _with_token(
        {
            "running": reported.running,
            "pausedUntil": reported.paused_until,
            "desiredRunning": desired_running,
            "desiredPausedUntil": desired_until,
            "settled": settled,
            "micReachable": True,
            "stateToken": "",
        }
    )


# Long-poll bounds: never hold a request past the cap (proxies and threadpools need
# a horizon), and re-derive the state every slice while hanging — transitions with no
# notify (a pause elapsing, a report aging into micReachable=False, a break-glass CLI
# pause writing the file directly) surface within a slice.
_WAIT_CAP_S = 25.0
_WAIT_SLICE_S = 2.0


def _capture_snapshot() -> CaptureOut:
    now = datetime.now(UTC)
    if capture_control.is_fleet():
        store = _store()
        try:
            return fleet_capture_state(store, now)
        finally:
            store.close()
    running = capture_control.capture_running() and not capture_control.is_paused(
        _root(), now
    )
    return _capture_state(running)


def capture_status(wait: float = 0, known: str = "") -> CaptureOut:
    """Whether the always-on capture is recording, and (if paused) when it
    auto-resumes by. Agents self-gate, so they stay loaded while paused — "running"
    means the capture agent is loaded *and* not currently paused. On the fleet (Isis)
    there is no local agent: it reports the Mac's mirrored state instead.

    Long-poll: with ?wait=<seconds>&known=<stateToken>, the request hangs while the
    state still fingerprints to `known` — a press or a confirming mirror report
    wakes it in ~RTT (capture_control.notify_capture_changed) instead of a poll
    interval. Without the params (an older client) it answers immediately."""
    # The version is read BEFORE each snapshot: a change landing mid-snapshot
    # makes the next wait return at once instead of being lost to the gap.
    seen = capture_control.capture_change_version()
    state = _capture_snapshot()
    deadline = time.monotonic() + min(wait, _WAIT_CAP_S)
    while state["stateToken"] == known:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            break
        capture_control.wait_capture_changed(min(_WAIT_SLICE_S, remaining), seen=seen)
        seen = capture_control.capture_change_version()
        state = _capture_snapshot()
    return state


def _record_control_origin(store: Store, verb: str, request: Request) -> None:
    """Durably record who asked for a pause/resume (#1347). Capture-control is
    login-free on the recording plane, so the agent's PAUSE/RESUME cannot name the
    caller; this writes an AUDIT-only CONTROL_REQUEST event carrying the request_origin
    descriptor. Best-effort: an audit write must never fail the control action itself,
    so any error is logged and swallowed."""
    try:
        origin = request_origin(
            WebAuthConfig.from_env(),
            method=request.method,
            path=request.url.path,
            cookie=request.cookies.get(COOKIE_NAME),
            authorization=request.headers.get("authorization"),
            client_host=request.client.host if request.client else None,
            now=datetime.now(UTC),
        )
        store.add_capture_event(
            capture_control.CaptureEventKind.CONTROL_REQUEST,
            utc=datetime.now(UTC),
            detail=f"{verb} — {origin}",
        )
        _log.info("%s requested by %s", verb.upper(), origin)
    except Exception:  # audit annotation must never break the control action
        _log.exception("could not record capture-control origin (%s)", verb)


def capture_pause(request: Request) -> CaptureOut:
    """Stop capture so the room can be worked in without recording. Bounded: it
    auto-resumes by the returned time even if left. On the fleet this records *intent*
    the Mac mirrors onto the mic; on the Mac it writes the local pause file directly."""
    now = datetime.now(UTC)
    if capture_control.is_fleet():
        store = _store()
        try:
            capture_control.intent_pause(store, now)
            _record_control_origin(store, "pause", request)
            state = fleet_capture_state(store, now)
        finally:
            store.close()
        _log.info("PAUSE intent recorded (fleet)")
        # Wake the hanging mirror exchange (intent changed) and every hanging
        # client poll — the press propagates in ~RTT, not a poll interval.
        capture_control.notify_capture_changed()
        # Desired just flipped; confirmed lags until the Mac applies — the client
        # shows "Pausing…", and the next poll returns this same shape (no flap).
        return state
    capture_control.pause(_root(), now)
    store = _store()
    try:
        _record_control_origin(store, "pause", request)
    finally:
        store.close()
    capture_control.notify_capture_changed()
    return _capture_state(running=False)


def capture_resume(request: Request) -> CaptureOut:
    """Start capture again now."""
    if capture_control.is_fleet():
        store = _store()
        try:
            capture_control.intent_resume(store)
            _record_control_origin(store, "resume", request)
            state = fleet_capture_state(store, datetime.now(UTC))
        finally:
            store.close()
        _log.info("RESUME intent recorded (fleet)")
        capture_control.notify_capture_changed()
        return state
    capture_control.resume(_root())
    store = _store()
    try:
        _record_control_origin(store, "resume", request)
    finally:
        store.close()
    capture_control.notify_capture_changed()
    return _capture_state(running=True)
