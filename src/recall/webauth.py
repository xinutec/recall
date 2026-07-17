"""Nextcloud SSO for the human-facing web UI — inert unless configured.

Like `recall.sync`, importing this changes nothing: the gate only goes up when the
OAuth env is set (`RECALL_SESSION_SECRET` + `NC_CLIENT_ID` + `NC_CLIENT_SECRET`). So the
Mac's LAN-only web UI and every test keep running open, and only the Isis fleet pod —
where the secret lives — raises the wall. That mirrors "sync is inert without a token".

Two planes, deliberately:

* **Browsing plane** — the Angular SPA and its read/write `/api/*` routes are gated on a
  Nextcloud sign-in. A request without a valid session gets `401 {"error": "not
  authenticated"}`; the SPA turns that into a "Sign in with Nextcloud" wall (same shape
  as health-sync).
* **Recording plane** — the paths a headless device or daemon uses, which *cannot* do an
  interactive OAuth login, stay open (network-gated by WireGuard/LAN, as before): the
  token-gated `/sync/*` ingest (its own bearer auth, untouched here) and the iOS mic
  app's capture control/liveness (`GET /api/capture`, `GET /api/sources`, and — by
  explicit choice — `POST /api/capture/pause|resume` so the phone keeps its button).

Identity only: the OAuth access token is used once to look up who the user is, then
discarded. There is no local user store — a signed, stateless cookie carries the
identity, and an optional username allowlist (`RECALL_ALLOWED_USERS`) restricts who may
enter even after a valid Nextcloud sign-in (empty = any authenticated NC user).
"""

from __future__ import annotations

import base64
import hashlib
import hmac
import json
import logging
import os
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta

import httpx
from fastapi import FastAPI, Request
from fastapi.responses import JSONResponse, RedirectResponse, Response

_log = logging.getLogger("recall.webauth")

SESSION_SECRET_ENV = "RECALL_SESSION_SECRET"
CLIENT_ID_ENV = "NC_CLIENT_ID"
CLIENT_SECRET_ENV = "NC_CLIENT_SECRET"
NC_BASE_URL_ENV = "NC_BASE_URL"
REDIRECT_URI_ENV = "NC_REDIRECT_URI"
ALLOWED_USERS_ENV = "RECALL_ALLOWED_USERS"

_DEFAULT_NC_BASE_URL = "https://dash.xinutec.org"
_DEFAULT_REDIRECT_URI = "http://10.100.0.2:8000/auth/callback"

COOKIE_NAME = "recall_session"
_SESSION_TTL = timedelta(days=7)
_STATE_TTL = timedelta(minutes=10)

# The recording plane: (method, path) pairs that stay open even with the gate up,
# because the client is a device/daemon that can't sign in interactively. Everything
# else under /api/* requires a session. `/sync/*` is not /api/* and carries its own
# bearer token, so it never reaches this middleware's gate.
_DEVICE_EXEMPT: frozenset[tuple[str, str]] = frozenset(
    {
        ("GET", "/api/capture"),  # iOS mic app long-polls household pause state
        ("GET", "/api/sources"),  # fleet mic-liveness view
        ("POST", "/api/capture/pause"),  # phone's pause button (login-free by choice)
        ("POST", "/api/capture/resume"),  # phone's resume button
        ("POST", "/api/log"),  # browser-side error log — usable from the sign-in wall
    }
)


def requires_session(method: str, path: str) -> bool:
    """True when this request must carry a valid session. Only the browsing plane
    (`/api/*` minus the device allowlist) is gated; static assets, the OAuth routes, and
    `/sync/*` are not."""
    if not path.startswith("/api/"):
        return False
    return (method.upper(), path) not in _DEVICE_EXEMPT


def validate_return_to(raw: str | None) -> str:
    """A safe local redirect target — a single-slash absolute path only. Anything that
    could leave the origin (`//host`, `https://…`, a scheme) collapses to `/`, so a
    crafted `?return_to=` can't turn login into an open redirect."""
    if not raw or not raw.startswith("/") or raw.startswith("//"):
        return "/"
    return raw


@dataclass(frozen=True)
class WebAuthConfig:
    """Everything the SSO gate needs. Absent (`from_env` → None) means the gate is
    off — recall runs as an open LAN web UI, exactly as before this module existed."""

    session_secret: str
    client_id: str
    client_secret: str
    nc_base_url: str
    redirect_uri: str
    allowed_users: frozenset[str]

    @classmethod
    def from_env(cls) -> WebAuthConfig | None:
        """Build from the environment, or None when SSO isn't configured. All three of
        the secret/id/secret must be present — a partial config is treated as off (and
        logged) rather than a half-raised, bypassable gate."""
        secret = os.environ.get(SESSION_SECRET_ENV)
        client_id = os.environ.get(CLIENT_ID_ENV)
        client_secret = os.environ.get(CLIENT_SECRET_ENV)
        present = [bool(secret), bool(client_id), bool(client_secret)]
        if not any(present):
            return None
        if not all(present):
            _log.warning(
                "web auth partially configured (%s/%s/%s set); gate stays OFF",
                SESSION_SECRET_ENV,
                CLIENT_ID_ENV,
                CLIENT_SECRET_ENV,
            )
            return None
        assert secret and client_id and client_secret  # narrowed by the checks above
        allowed = {
            u.strip()
            for u in os.environ.get(ALLOWED_USERS_ENV, "").split(",")
            if u.strip()
        }
        return cls(
            session_secret=secret,
            client_id=client_id,
            client_secret=client_secret,
            nc_base_url=os.environ.get(NC_BASE_URL_ENV, _DEFAULT_NC_BASE_URL).rstrip(
                "/"
            ),
            redirect_uri=os.environ.get(REDIRECT_URI_ENV, _DEFAULT_REDIRECT_URI),
            allowed_users=frozenset(allowed),
        )

    def permits(self, user_id: str) -> bool:
        """Whether a signed-in Nextcloud user may enter. Empty allowlist = any
        authenticated user; otherwise an exact-match allowlist."""
        return not self.allowed_users or user_id in self.allowed_users


@dataclass(frozen=True)
class Session:
    """Who is signed in, carried entirely in the signed cookie (no server store)."""

    user_id: str
    display_name: str


def _b64e(raw: bytes) -> str:
    return base64.urlsafe_b64encode(raw).rstrip(b"=").decode("ascii")


def _b64d(text: str) -> bytes:
    pad = "=" * (-len(text) % 4)
    return base64.urlsafe_b64decode(text + pad)


def _sign(
    secret: str, payload: dict[str, object], now: datetime, ttl: timedelta
) -> str:
    """A `<payload>.<mac>` token: the payload (with an `exp` epoch) base64url-encoded,
    and an HMAC-SHA256 of it keyed by the secret. Stateless — verification needs only
    the secret, no store."""
    body = dict(payload)
    body["exp"] = int((now + ttl).timestamp())
    encoded = _b64e(json.dumps(body, separators=(",", ":"), sort_keys=True).encode())
    mac = hmac.new(secret.encode(), encoded.encode(), hashlib.sha256).digest()
    return f"{encoded}.{_b64e(mac)}"


def _verify(secret: str, token: str, now: datetime) -> dict[str, object] | None:
    """The payload if the token's HMAC checks out and it hasn't expired, else None.
    Constant-time MAC compare; any malformed input returns None rather than raising."""
    try:
        encoded, presented = token.split(".")
    except ValueError:
        return None
    expected = hmac.new(secret.encode(), encoded.encode(), hashlib.sha256).digest()
    try:
        if not hmac.compare_digest(expected, _b64d(presented)):
            return None
        payload = json.loads(_b64d(encoded))
    except (ValueError, json.JSONDecodeError):
        return None
    if not isinstance(payload, dict):
        return None
    exp = payload.get("exp")
    if not isinstance(exp, int) or exp < int(now.timestamp()):
        return None
    return payload


def make_session_cookie(cfg: WebAuthConfig, session: Session, now: datetime) -> str:
    return _sign(
        cfg.session_secret,
        {"uid": session.user_id, "name": session.display_name},
        now,
        _SESSION_TTL,
    )


def read_session_cookie(
    cfg: WebAuthConfig, token: str | None, now: datetime
) -> Session | None:
    if not token:
        return None
    payload = _verify(cfg.session_secret, token, now)
    if payload is None:
        return None
    uid, name = payload.get("uid"), payload.get("name")
    if not isinstance(uid, str) or not isinstance(name, str):
        return None
    return Session(user_id=uid, display_name=name)


def make_state(cfg: WebAuthConfig, return_to: str | None, now: datetime) -> str:
    return _sign(
        cfg.session_secret, {"rt": validate_return_to(return_to)}, now, _STATE_TTL
    )


def read_state(cfg: WebAuthConfig, token: str, now: datetime) -> str | None:
    """The validated return_to path if the state token is authentic and fresh, else
    None (rejected login — expired or forged state)."""
    payload = _verify(cfg.session_secret, token, now)
    if payload is None:
        return None
    rt = payload.get("rt")
    return validate_return_to(rt if isinstance(rt, str) else None)


def authorize_url(cfg: WebAuthConfig, state: str) -> str:
    params = httpx.QueryParams(
        {
            "client_id": cfg.client_id,
            "response_type": "code",
            "redirect_uri": cfg.redirect_uri,
            "state": state,
        }
    )
    return f"{cfg.nc_base_url}/index.php/apps/oauth2/authorize?{params}"


def exchange_code(cfg: WebAuthConfig, code: str) -> str:
    """Trade the authorization code for an access token at Nextcloud's OAuth2 route."""
    resp = httpx.post(
        f"{cfg.nc_base_url}/index.php/apps/oauth2/api/v1/token",
        data={
            "grant_type": "authorization_code",
            "code": code,
            "client_id": cfg.client_id,
            "client_secret": cfg.client_secret,
            "redirect_uri": cfg.redirect_uri,
        },
        timeout=15.0,
    )
    resp.raise_for_status()
    token = resp.json().get("access_token")
    if not isinstance(token, str) or not token:
        raise ValueError("nextcloud token response missing access_token")
    return token


def fetch_userinfo(cfg: WebAuthConfig, access_token: str) -> Session:
    """Look up the signed-in user (id + display name) via the OCS user endpoint. The
    access token is used here once and then discarded — identity-only."""
    resp = httpx.get(
        f"{cfg.nc_base_url}/ocs/v2.php/cloud/user?format=json",
        headers={
            "Authorization": f"Bearer {access_token}",
            "OCS-APIRequest": "true",
        },
        timeout=15.0,
    )
    resp.raise_for_status()
    data = resp.json().get("ocs", {}).get("data", {})
    uid = data.get("id")
    if not isinstance(uid, str) or not uid:
        raise ValueError("nextcloud user response missing id")
    name = data.get("displayname")
    return Session(user_id=uid, display_name=name if isinstance(name, str) else uid)


def _now() -> datetime:
    return datetime.now(UTC)


def register_web_auth(app: FastAPI, config: WebAuthConfig | None = None) -> bool:
    """Raise the Nextcloud SSO gate on `app`, but only when configured — so a stock
    (Mac / dev / test) deployment is unchanged. `config` defaults to `from_env`; pass it
    explicitly in tests. Returns whether the gate was installed."""
    cfg = config if config is not None else WebAuthConfig.from_env()
    if cfg is None:
        return False

    @app.middleware("http")
    async def _gate(
        request: Request, call_next: Callable[[Request], Awaitable[Response]]
    ) -> Response:
        if not requires_session(request.method, request.url.path):
            return await call_next(request)
        session = read_session_cookie(cfg, request.cookies.get(COOKIE_NAME), _now())
        if session is None:
            return JSONResponse({"error": "not authenticated"}, status_code=401)
        if not cfg.permits(session.user_id):
            return JSONResponse({"error": "not authorised"}, status_code=403)
        return await call_next(request)

    @app.get("/login")
    def login(return_to: str | None = None) -> RedirectResponse:
        """Kick off the Nextcloud sign-in, remembering where to land afterwards."""
        state = make_state(cfg, return_to, _now())
        return RedirectResponse(authorize_url(cfg, state), status_code=302)

    @app.get("/auth/callback")
    def callback(code: str | None = None, state: str | None = None) -> Response:
        """Nextcloud redirects back here with a code; verify state, resolve identity,
        enforce the allowlist, and set the session cookie."""
        return_to = read_state(cfg, state, _now()) if state else None
        if return_to is None:
            return JSONResponse(
                {"error": "invalid or expired login state"}, status_code=403
            )
        if not code:
            return JSONResponse(
                {"error": "missing authorization code"}, status_code=400
            )
        try:
            session = fetch_userinfo(cfg, exchange_code(cfg, code))
        except (httpx.HTTPError, ValueError):
            _log.exception("nextcloud sign-in failed")
            return JSONResponse({"error": "sign-in failed"}, status_code=502)
        if not cfg.permits(session.user_id):
            return JSONResponse(
                {"error": f"{session.user_id} is not permitted"}, status_code=403
            )
        resp = RedirectResponse(return_to, status_code=302)
        resp.set_cookie(
            COOKIE_NAME,
            make_session_cookie(cfg, session, _now()),
            max_age=int(_SESSION_TTL.total_seconds()),
            httponly=True,
            samesite="lax",
            # Not Secure: recall answers over plain http on the WireGuard IP; the
            # network is the real gate. Revisit if it ever gains an https origin.
            secure=False,
            path="/",
        )
        return resp

    @app.post("/logout")
    def logout() -> RedirectResponse:
        resp = RedirectResponse("/", status_code=302)
        resp.delete_cookie(COOKIE_NAME, path="/")
        return resp

    @app.get("/api/me")
    def me(request: Request) -> JSONResponse:
        """Who is signed in — the SPA's login probe. The gate returns 401 before this
        runs when there's no session, so reaching it means a valid session exists."""
        session = read_session_cookie(cfg, request.cookies.get(COOKIE_NAME), _now())
        assert session is not None  # the gate guarantees a session on /api/*
        return JSONResponse(
            {"userId": session.user_id, "displayName": session.display_name}
        )

    return True
