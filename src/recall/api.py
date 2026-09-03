"""FastAPI JSON API over the recall core (consumed by the Angular front-end).

A thin transport layer: every endpoint calls the typed core (store / review /
speakerid) and returns JSON. FastAPI/pydantic are waived in mypy (present only in
the venv), so keep logic in the core — not here. When a built Angular app exists
at frontend/dist/, it's served as static files so everything is one origin.
"""

from __future__ import annotations

import logging
from datetime import UTC, datetime
from pathlib import Path

from fastapi import FastAPI, HTTPException
from fastapi.responses import FileResponse

from recall.api_audio import clip_window, register_audio_routes
from recall.api_capture import fleet_capture_state, register_capture_routes
from recall.api_client_reports import register_client_report_routes
from recall.api_devices import register_device_routes
from recall.api_experiments import register_experiment_routes
from recall.api_labels import register_label_routes
from recall.api_quiet import register_quiet_routes
from recall.api_reads import register_read_routes
from recall.api_recall import register_recall_routes
from recall.api_sessions import register_session_routes
from recall.paths import default_data_root
from recall.store import (
    Store,
)
from recall.sync import register_sync_routes
from recall.webauth import (
    register_web_auth,
)

DATA_ROOT = default_data_root()
_log = logging.getLogger("recall.api")
# Train pre-fills "sounds like X" only when the leading candidate's likelihood
# (softmax over the enrolled people) clears this — a confirmable hint, not a coin
# flip. The timeline still shows every guess with its %.
_SUGGEST_MIN_PROB = 0.4
_REPO = Path(__file__).resolve().parent.parent.parent
_FRONTEND = _REPO / "frontend" / "dist" / "recall-web" / "browser"


app = FastAPI(title="recall")


def _store() -> Store:
    return Store.open(DATA_ROOT / "recall.sqlite")


# Mac→fleet sync endpoints for the proposed Isis split (recall.sync). Inert unless
# RECALL_SYNC_TOKEN is set — register_sync_routes adds nothing and returns False — so a
# stock LAN-only deployment is unchanged.
register_sync_routes(app, _store, DATA_ROOT)

# Nextcloud SSO gate over the human-facing web UI (recall.webauth). Also inert unless
# configured (RECALL_SESSION_SECRET + NC_CLIENT_ID + NC_CLIENT_SECRET), so the Mac's
# LAN-only UI stays open; only the Isis fleet pod, where the secret lives, raises the
# wall. The recording plane (/sync/* and the iOS mic app's capture endpoints) is exempt.
register_web_auth(app)


register_read_routes(app, store_factory=_store, parse_iso=lambda v: _parse_iso(v))  # noqa: PLW0108 - forward ref
register_client_report_routes(app, client_log_path=_REPO / "logs" / "client.log")


register_capture_routes(app, store_factory=_store, data_root=lambda: DATA_ROOT)
register_device_routes(
    app,
    store_factory=_store,
    data_root=lambda: DATA_ROOT,
    fleet_capture_state=fleet_capture_state,
)
register_session_routes(
    app,
    store_factory=_store,
    data_root=lambda: DATA_ROOT,
    # a lambda, deliberately: _require_time is defined further down this module
    require_time=lambda value: _require_time(value),  # noqa: PLW0108 - forward ref
)


def _parse_iso(value: str | None) -> datetime | None:
    if value is None:
        return None
    parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    return parsed if parsed.tzinfo else parsed.replace(tzinfo=UTC)


def _require_time(value: str | None) -> datetime:
    parsed = _parse_iso(value)
    if parsed is None:
        msg = "a valid ISO 8601 time is required"
        raise ValueError(msg)
    return parsed


register_label_routes(
    app,
    store_factory=_store,
    parse_iso=_parse_iso,
    require_time=_require_time,
    clip_window_fn=clip_window,
)
register_audio_routes(app, store_factory=_store)
register_recall_routes(
    app,
    store_factory=_store,
    # a lambda, deliberately: _require_time is defined further down this module
    require_time=lambda value: _require_time(value),  # noqa: PLW0108 - forward ref
)
register_experiment_routes(
    app,
    store_factory=_store,
    require_time=_require_time,
    parse_iso=_parse_iso,
)
register_quiet_routes(app, store_factory=_store, require_time=_require_time)


def _frontend_file(rel: str) -> Path | None:
    """Resolve `rel` to a real file inside the built frontend, or None.

    Guards against path traversal: the resolved path must stay under _FRONTEND.
    """
    if not _FRONTEND.is_dir():
        return None
    candidate = (_FRONTEND / rel).resolve()
    root = _FRONTEND.resolve()
    if candidate != root and root not in candidate.parents:
        return None
    return candidate if candidate.is_file() else None


@app.get("/{full_path:path}")
def spa(full_path: str) -> FileResponse:
    """Serve built assets; fall back to index.html so client-side routes work.

    Registered last, so the explicit /api/* routes above always win. API paths
    that fall through here are genuine misses and return 404 (not index.html).
    """
    if full_path.startswith("api/"):
        raise HTTPException(status_code=404, detail="not found")
    asset = _frontend_file(full_path)
    if asset is not None:
        # Built assets are content-hashed (main-<hash>.js), so they're immutable —
        # cache them hard.
        return FileResponse(
            asset,
            headers={"Cache-Control": "public, max-age=31536000, immutable"},
        )
    index = _frontend_file("index.html")
    if index is None:
        raise HTTPException(status_code=404, detail="frontend not built")
    # index.html names the current hashed bundles, so it must never be cached — else a
    # deploy isn't picked up until a hard refresh (the bug that served stale code).
    return FileResponse(index, headers={"Cache-Control": "no-cache"})
