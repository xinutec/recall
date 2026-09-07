"""Every route the app calls must be served by SOMEBODY.

⚠ This exists because a route was served by nobody for one deploy. `/api/correct`
was deleted from the Python in the same change that ported it to recalld, and the
Rust handler was written and tested but never mounted. Both halves were green:
recalld's tests exercised `apply_correction` directly, and Python's suite could
not miss a route it no longer had. The frontend's correct button simply stopped
working, answered by the SPA catch-all with a 405.

⚠ **The existing wire lint cannot catch this.** DL-WIRE-ROUTE-DRIFT resolves
recalld's axum table against the frontend's call sites, which is the right check
for a finished port and the wrong one during a strangler: while the proxy stands,
"not in the axum table" is a legitimate answer meaning "Python still serves it".
Only the UNION of the two tables decides whether anyone answers, and only this
repo can see both.
"""

from __future__ import annotations

import re
from pathlib import Path

from recall.api import app

_REPO = Path(__file__).resolve().parent.parent
_FRONTEND_API = _REPO / "frontend" / "src" / "app" / "recall-api.ts"
_ROUTER = _REPO / "recalld" / "src" / "app.rs"

# `this.http.post<T>('/api/correct'` / with a template literal and interpolation.
_CALL = re.compile(
    r"this\.http\.(?P<method>get|post|put|patch|delete)<[^>]*>\(\s*[`'](?P<path>[^`']*)[`']"
)
# `.route("/api/correct", post(...))` — the fleet-standard literal form.
_ROUTE = re.compile(r'\.route\(\s*\n?\s*"(?P<path>/[^"]*)"')


def _normalise(path: str) -> str:
    """Reduce a call or a route to a comparable shape.

    Drops the query string, and rewrites both languages' parameter spellings —
    `${id}` and `{id}` — to a single placeholder, so `/api/audio/${id}` and
    `/api/audio/{id}` are one path.

    ⚠ An interpolation only counts as a path parameter when it FOLLOWS a slash.
    `/api/capture${query}` appends a query string, not a segment; reading it as
    one invents a route nobody serves and the check cries wolf on its first run
    (it did).
    """
    path = path.split("?", 1)[0]
    path = re.sub(r"(?<=/)\$\{[^}]*\}", "{}", path)
    path = re.sub(r"\$\{[^}]*\}", "", path)
    path = re.sub(r"\{[^}]*\}", "{}", path)
    return path.rstrip("/") or "/"


def _frontend_calls() -> set[str]:
    text = _FRONTEND_API.read_text()
    return {
        _normalise(m.group("path"))
        for m in _CALL.finditer(text)
        if m.group("path").startswith("/api/")
    }


def _recalld_routes() -> set[str]:
    text = _ROUTER.read_text()
    return {
        _normalise(m.group("path"))
        for m in _ROUTE.finditer(text)
        if m.group("path").startswith("/api/")
    }


def _python_routes() -> set[str]:
    paths = (str(getattr(route, "path", "")) for route in app.routes)
    return {_normalise(path) for path in paths if path.startswith("/api/")}


def test_every_api_route_the_app_calls_is_served_by_somebody() -> None:
    called = _frontend_calls()
    assert called, "the call-site scan found nothing — the parser has drifted"

    served = _recalld_routes() | _python_routes()
    orphaned = sorted(called - served)

    assert not orphaned, (
        f"{len(orphaned)} route(s) the app calls are served by NEITHER half: "
        f"{orphaned}. A route deleted from Python must be mounted in recalld's "
        f"router in the same change — writing the handler is not mounting it."
    )


def test_the_axum_route_scan_still_matches_something() -> None:
    """A guard on the guard: a regex that silently matched nothing would make the
    coverage test above pass by finding no orphans in an empty set.

    ⚠ Only the axum half is guarded, and only this half needs it. It is parsed
    from TEXT, so a change to how routes are written — a macro, a different
    builder — makes it match nothing while still looking healthy. The FastAPI
    half reads `app.routes` off the live object: it cannot silently mismatch, and
    it is ALLOWED to reach zero, because zero is where the port is going.

    ⚠ This once asserted the Python half saw more than five routes. That was a
    snapshot of an afternoon, not a property, and it failed the moment the port
    passed it — a guard that has to be edited every time the thing it guards
    makes progress is measuring the wrong thing.
    """
    assert len(_recalld_routes()) > 10, "the axum route scan has drifted"
    # A route every version of this router has served, as a canary for the shape.
    assert "/api/timeline" in _recalld_routes(), "the axum scan lost a known route"
