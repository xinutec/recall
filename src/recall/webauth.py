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
* **Device-token plane** — paths a device may use, but only with a credential. A
  `RECALL_DEVICE_TOKEN` bearer is accepted *instead of* a cookie on these, and nowhere
  else.

The third plane exists because the second one could not answer the Android meeting
recorder. `POST /api/sessions` is the endpoint it uploads to; the browser's Upload
button reaches it with a cookie, and the phone has none — the WebView that signs in is
a separate app (`org.recall.web`) with its own cookie jar. Until 2026-08-07 every
meeting upload got a 401, silently and forever, retried by WorkManager against a wall.

The alternative was to add the path to the exempt set above, and that set can only ever
say *no credential at all*. Pausing a microphone is a fair thing to leave login-free;
accepting tens of megabytes and creating a session is not the same trade, so the grant
is a token rather than an exemption. The token authorises those paths **only** — a
phone that can upload a recording still cannot read the household's transcripts, which
a session cookie would have let it do.

Deliberately NOT the sync token: that one opens the whole `/sync/*` surface — the
archive push, the job pull, the path-checked writes — and a phone is easier to lose
than a Mac. Two secrets, rotated independently.

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
from urllib.parse import urlsplit

import httpx
from fastapi import FastAPI, Request
from fastapi.responses import JSONResponse, RedirectResponse, Response

_log = logging.getLogger("recall.webauth")

SESSION_SECRET_ENV = "RECALL_SESSION_SECRET"
CLIENT_ID_ENV = "NC_CLIENT_ID"
CLIENT_SECRET_ENV = "NC_CLIENT_SECRET"
NC_BASE_URL_ENV = "NC_BASE_URL"
NC_INTERNAL_URL_ENV = "NC_INTERNAL_URL"
REDIRECT_URI_ENV = "NC_REDIRECT_URI"
ALLOWED_USERS_ENV = "RECALL_ALLOWED_USERS"
DEVICE_TOKEN_ENV = "RECALL_DEVICE_TOKEN"

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
        # ⚠ The phone saying what it could NOT upload (#77), and it MUST be here
        # rather than on the device-token plane. It reports with the same credential
        # it uploads with, so gating it means that when the token is wrong — the
        # 2026-08-07 incident, and the single most likely fault — the report is 401ed
        # too and the fleet learns nothing. The one failure the check exists to catch
        # would be the one it cannot see. Found by running it: the phone wrote "Not
        # authorised — check the upload token" to its own row while `/sync/devices/
        # outbox` still returned `{"items":[]}`.
        #
        # Same trade as `/api/log` directly above: a report about being unable to
        # authenticate cannot require authentication. What it costs is that anyone
        # already inside WireGuard/the LAN can lie about a queue depth in a health
        # check — no read access, no audio, no archive.
        ("POST", "/api/devices/outbox"),
        # The mic app saying it is still running (#837), on the same terms and for a
        # sharper version of the same reason. This one carries NO credential at all:
        # the mic app streams PCM over a bare TCP socket and has never held a token,
        # and inventing one for a liveness beat would mean a phone whose credential
        # went bad reads as DEAD — turning a config mistake into a false alarm about
        # the hardware, which is the failure mode the beat exists to rule out.
        #
        # What it costs: anyone already inside WireGuard/the LAN can post a beat for
        # any device id, so this can be made to look healthier than it is. It cannot
        # be made to look worse (a beat only ever refreshes), it grants no read, no
        # audio and no archive, and the device list is capped and evicted by age so a
        # flood is bounded (recall.mic_alive.MAX_DEVICES).
        ("POST", "/api/devices/heartbeat"),
    }
)


# The device-token plane: (method, path) pairs where a `RECALL_DEVICE_TOKEN` bearer is
# accepted in place of a session cookie. A cookie still works — this widens who may call
# these, it does not narrow it.
#
# Kept as a closed set rather than "a token is as good as a session anywhere", so the
# credential on the phone grants exactly what the phone does. Adding a path here is a
# deliberate act with a blast radius to weigh; that is the point of it being a list.
_DEVICE_TOKEN_PATHS: frozenset[tuple[str, str]] = frozenset(
    {
        ("POST", "/api/sessions"),  # Android meeting recorder + share sheet upload
    }
)


def requires_session(method: str, path: str) -> bool:
    """True when this request must carry a valid session. Only the browsing plane
    (`/api/*` minus the device allowlist) is gated; static assets, the OAuth routes, and
    `/sync/*` are not."""
    if not path.startswith("/api/"):
        return False
    return (method.upper(), path) not in _DEVICE_EXEMPT


def accepts_device_token(method: str, path: str) -> bool:
    """True when a device bearer token may stand in for a session on this route."""
    return (method.upper(), path) in _DEVICE_TOKEN_PATHS


def request_origin(  # noqa: PLR0913 - the request fields it reads, kept flat + pure
    cfg: WebAuthConfig | None,
    *,
    method: str,
    path: str,
    cookie: str | None,
    authorization: str | None,
    client_host: str | None,
    now: datetime,
) -> str:
    """A short, durable descriptor of who asked for a capture-control action — the
    answer to "was that pause mine?" (#1347). Capture-control paths are login-free on
    the recording plane, so the request carries no identity the gate enforced; this
    reconstructs what it *would* have found: the signed-in user if a valid cookie is
    present, else the device-token plane if a token was accepted on this route, else an
    anonymous peer. With auth off (Mac / dev / LAN-only) there is no plane, so only the
    peer address is known. Pure and total: it reads what the request already carried and
    never raises, so annotating a pause can never break the pause.
    """
    host = client_host or "unknown-host"
    if cfg is None:
        return f"no-auth {host}"
    session = read_session_cookie(cfg, cookie, now)
    if session is not None:
        return f"user {session.user_id} {host}"
    if cfg.presents_device_token(method, path, authorization):
        return f"device-token {host}"
    return f"anon {host}"


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
    # Public, browser-facing base URL (the authorize redirect + redirect_uri go here).
    nc_base_url: str
    # Where the *server* reaches Nextcloud for the token + userinfo calls. Usually the
    # same as nc_base_url, but on the fleet the pod can't reach the public host (it is
    # the node's own IP → hairpin), so this points at Nextcloud's in-cluster Service DNS
    # name and the calls carry a Host header of the public host. Defaults to the public.
    nc_internal_url: str
    redirect_uri: str
    allowed_users: frozenset[str]
    # The bearer a device may present instead of a cookie, on `_DEVICE_TOKEN_PATHS`
    # alone. None (the default) leaves those routes cookie-only, so a deployment that
    # does not set it behaves exactly as it did before the plane existed.
    device_token: str | None = None

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
        nc_base_url = os.environ.get(NC_BASE_URL_ENV, _DEFAULT_NC_BASE_URL).rstrip("/")
        internal = os.environ.get(NC_INTERNAL_URL_ENV, "").rstrip("/") or nc_base_url
        return cls(
            session_secret=secret,
            client_id=client_id,
            client_secret=client_secret,
            nc_base_url=nc_base_url,
            nc_internal_url=internal,
            redirect_uri=os.environ.get(REDIRECT_URI_ENV, _DEFAULT_REDIRECT_URI),
            allowed_users=frozenset(allowed),
            # Optional, and independent of the three above: unset simply means no
            # device may upload. It is NOT part of the `present`/`all` check, because a
            # missing device token is a smaller deployment rather than a broken one.
            device_token=os.environ.get(DEVICE_TOKEN_ENV) or None,
        )

    def server_call(self, path: str) -> tuple[str, dict[str, str]]:
        """The (url, headers) for a *server-side* Nextcloud call at `path`. When the
        internal URL differs from the public one, the request goes to the in-cluster
        address but presents the public host as `Host:` so Nextcloud's trusted-domain
        routing treats it exactly like the public request."""
        url = f"{self.nc_internal_url}{path}"
        if self.nc_internal_url == self.nc_base_url:
            return url, {}
        return url, {"Host": urlsplit(self.nc_base_url).netloc}

    def permits(self, user_id: str) -> bool:
        """Whether a signed-in Nextcloud user may enter. Empty allowlist = any
        authenticated user; otherwise an exact-match allowlist."""
        return not self.allowed_users or user_id in self.allowed_users

    def presents_device_token(
        self, method: str, path: str, authorization: str | None
    ) -> bool:
        """Whether this request carries the device token on a route that accepts one.

        Constant-time compare, like `sync.check_token`: a wrong token must leak no
        timing signal. Returns a bool rather than raising, because a device that fails
        here earns exactly the 401 an absent cookie earns and should learn nothing more
        about which of the two it missed.

        The `Bearer ` parse is deliberately not imported from `recall.sync`, which has
        the identical helper. Keeping it here is what keeps this module a leaf: the
        request-authorisation path should not pull in the store, the timeline and httpx
        by way of the sync plane. The duplicated part is a fixed header format, not a
        policy that can drift.
        """
        if self.device_token is None or not accepts_device_token(method, path):
            return False
        prefix = "Bearer "
        if not authorization or not authorization.startswith(prefix):
            return False
        return hmac.compare_digest(authorization[len(prefix) :], self.device_token)


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
    url, headers = cfg.server_call("/index.php/apps/oauth2/api/v1/token")
    resp = httpx.post(
        url,
        data={
            "grant_type": "authorization_code",
            "code": code,
            "client_id": cfg.client_id,
            "client_secret": cfg.client_secret,
            "redirect_uri": cfg.redirect_uri,
        },
        headers=headers,
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
    url, headers = cfg.server_call("/ocs/v2.php/cloud/user?format=json")
    resp = httpx.get(
        url,
        headers={
            **headers,
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
            # A device with the token, on the routes that accept one. Checked only
            # after the cookie fails, so a signed-in browser is unaffected either way.
            if cfg.presents_device_token(
                request.method, request.url.path, request.headers.get("authorization")
            ):
                return await call_next(request)
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
