"""Nextcloud SSO gate for the web UI: the signing, the allowlist split, and the flow.

The security-critical parts are pinned: a tampered/expired/forged cookie or state is
rejected, the gate is absent unless configured, the browsing plane is gated while the
recording plane (device endpoints) stays open, and only allowlisted users get in.
"""

from __future__ import annotations

from datetime import UTC, datetime, timedelta

import pytest
from fastapi import FastAPI
from fastapi.testclient import TestClient

from recall import webauth
from recall.webauth import (
    ALLOWED_USERS_ENV,
    CLIENT_ID_ENV,
    CLIENT_SECRET_ENV,
    COOKIE_NAME,
    SESSION_SECRET_ENV,
    Session,
    WebAuthConfig,
    authorize_url,
    make_session_cookie,
    make_state,
    read_session_cookie,
    read_state,
    register_web_auth,
    requires_session,
    validate_return_to,
)

NOW = datetime(2026, 7, 17, 12, 0, 0, tzinfo=UTC)


def _cfg(allowed: set[str] | None = None) -> WebAuthConfig:
    return WebAuthConfig(
        session_secret="s3cret-key-material-0123456789",
        client_id="recall-client",
        client_secret="recall-secret",
        nc_base_url="https://dash.example",
        redirect_uri="http://10.100.0.2:8000/auth/callback",
        allowed_users=frozenset(allowed or set()),
    )


# --- signing --------------------------------------------------------------


def test_session_cookie_roundtrips() -> None:
    cfg = _cfg()
    token = make_session_cookie(cfg, Session("pippijn", "Pippijn"), NOW)
    got = read_session_cookie(cfg, token, NOW)
    assert got == Session("pippijn", "Pippijn")


def test_session_cookie_rejects_tampering() -> None:
    cfg = _cfg()
    token = make_session_cookie(cfg, Session("pippijn", "Pippijn"), NOW)
    forged = token[:-2] + ("aa" if not token.endswith("aa") else "bb")
    assert read_session_cookie(cfg, forged, NOW) is None


def test_session_cookie_rejects_a_different_secret() -> None:
    token = make_session_cookie(_cfg(), Session("pippijn", "Pippijn"), NOW)
    other = WebAuthConfig(
        session_secret="a-totally-different-secret-key",
        client_id="recall-client",
        client_secret="recall-secret",
        nc_base_url="https://dash.example",
        redirect_uri="http://10.100.0.2:8000/auth/callback",
        allowed_users=frozenset(),
    )
    assert read_session_cookie(other, token, NOW) is None


def test_session_cookie_expires() -> None:
    cfg = _cfg()
    token = make_session_cookie(cfg, Session("pippijn", "Pippijn"), NOW)
    assert read_session_cookie(cfg, token, NOW + timedelta(days=8)) is None


def test_read_session_cookie_none_for_missing_or_garbage() -> None:
    cfg = _cfg()
    assert read_session_cookie(cfg, None, NOW) is None
    assert read_session_cookie(cfg, "", NOW) is None
    assert read_session_cookie(cfg, "not-a-token", NOW) is None


def test_state_roundtrips_return_to() -> None:
    cfg = _cfg()
    token = make_state(cfg, "/timeline?date=2026-07-17", NOW)
    assert read_state(cfg, token, NOW) == "/timeline?date=2026-07-17"


def test_state_sanitises_open_redirect() -> None:
    cfg = _cfg()
    token = make_state(cfg, "//evil.example/phish", NOW)
    assert read_state(cfg, token, NOW) == "/"


def test_state_expires() -> None:
    cfg = _cfg()
    token = make_state(cfg, "/", NOW)
    assert read_state(cfg, token, NOW + timedelta(minutes=11)) is None


def test_state_rejects_forged_token() -> None:
    assert read_state(_cfg(), "garbage.token", NOW) is None


@pytest.mark.parametrize(
    ("raw", "expected"),
    [
        ("/timeline", "/timeline"),
        ("/", "/"),
        (None, "/"),
        ("", "/"),
        ("//evil", "/"),
        ("https://evil.example", "/"),
        ("javascript:alert(1)", "/"),
    ],
)
def test_validate_return_to(raw: str | None, expected: str) -> None:
    assert validate_return_to(raw) == expected


# --- the plane split ------------------------------------------------------


@pytest.mark.parametrize(
    ("method", "path", "gated"),
    [
        ("GET", "/api/transcripts", True),
        ("POST", "/api/correct", True),
        ("GET", "/api/me", True),
        ("GET", "/api/capture", False),  # phone long-poll
        ("GET", "/api/sources", False),  # fleet liveness
        ("POST", "/api/capture/pause", False),  # phone button, by choice
        ("POST", "/api/capture/resume", False),
        ("POST", "/api/log", False),
        ("POST", "/sync/audio", False),  # not /api/*, own bearer auth
        ("GET", "/", False),  # static SPA shell
        ("GET", "/timeline", False),  # SPA route
        ("GET", "/login", False),
    ],
)
def test_requires_session(method: str, path: str, gated: bool) -> None:
    assert requires_session(method, path) is gated


def test_pause_is_still_gated_under_a_different_method() -> None:
    # The allowlist is (method, path) exact — a GET to the pause path is not the phone's
    # POST, so it does not get the exemption.
    assert requires_session("GET", "/api/capture/pause") is True


# --- config ---------------------------------------------------------------


def test_from_env_off_when_unset(monkeypatch: pytest.MonkeyPatch) -> None:
    for var in (
        SESSION_SECRET_ENV,
        CLIENT_ID_ENV,
        CLIENT_SECRET_ENV,
        ALLOWED_USERS_ENV,
    ):
        monkeypatch.delenv(var, raising=False)
    assert WebAuthConfig.from_env() is None


def test_from_env_off_when_partial(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv(SESSION_SECRET_ENV, "x")
    monkeypatch.setenv(CLIENT_ID_ENV, "cid")
    monkeypatch.delenv(CLIENT_SECRET_ENV, raising=False)
    assert WebAuthConfig.from_env() is None


def test_from_env_builds_when_complete(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv(SESSION_SECRET_ENV, "secret")
    monkeypatch.setenv(CLIENT_ID_ENV, "cid")
    monkeypatch.setenv(CLIENT_SECRET_ENV, "csec")
    monkeypatch.setenv(ALLOWED_USERS_ENV, "pippijn, guest")
    cfg = WebAuthConfig.from_env()
    assert cfg is not None
    assert cfg.client_id == "cid"
    assert cfg.allowed_users == frozenset({"pippijn", "guest"})
    assert cfg.nc_base_url == "https://dash.xinutec.org"


def test_permits_allowlist() -> None:
    assert _cfg({"pippijn"}).permits("pippijn") is True
    assert _cfg({"pippijn"}).permits("eve") is False
    # Empty allowlist = any authenticated user.
    assert _cfg().permits("anyone") is True


def test_authorize_url_carries_params() -> None:
    url = authorize_url(_cfg(), "the-state")
    assert url.startswith("https://dash.example/index.php/apps/oauth2/authorize?")
    assert "client_id=recall-client" in url
    assert "response_type=code" in url
    assert "state=the-state" in url


# --- the gate, end to end -------------------------------------------------


def _app(cfg: WebAuthConfig) -> FastAPI:
    app = FastAPI()

    @app.get("/api/transcripts")
    def _transcripts() -> dict[str, str]:
        return {"ok": "browsing"}

    @app.get("/api/capture")
    def _capture() -> dict[str, str]:
        return {"ok": "device"}

    @app.get("/")
    def _root() -> dict[str, str]:
        return {"ok": "static"}

    assert register_web_auth(app, cfg) is True
    return app


def test_gate_blocks_browsing_without_a_session() -> None:
    client = TestClient(_app(_cfg({"pippijn"})))
    resp = client.get("/api/transcripts")
    assert resp.status_code == 401
    assert resp.json() == {"error": "not authenticated"}


def test_gate_lets_the_recording_plane_through() -> None:
    client = TestClient(_app(_cfg({"pippijn"})))
    assert client.get("/api/capture").status_code == 200
    assert client.get("/").status_code == 200


def test_login_redirects_to_nextcloud() -> None:
    client = TestClient(_app(_cfg({"pippijn"})))
    resp = client.get("/login", follow_redirects=False)
    assert resp.status_code == 302
    loc = resp.headers["location"]
    assert loc.startswith("https://dash.example/index.php/apps/oauth2/authorize?")
    assert "state=" in loc


def test_callback_rejects_bad_state() -> None:
    client = TestClient(_app(_cfg({"pippijn"})))
    resp = client.get("/auth/callback?code=abc&state=forged", follow_redirects=False)
    assert resp.status_code == 403


def test_callback_signs_in_allowed_user_and_unlocks_browsing(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    cfg = _cfg({"pippijn"})
    client = TestClient(_app(cfg))
    monkeypatch.setattr(webauth, "exchange_code", lambda _cfg, _code: "access-tok")
    monkeypatch.setattr(
        webauth, "fetch_userinfo", lambda _cfg, _tok: Session("pippijn", "Pippijn")
    )
    state = make_state(cfg, "/", datetime.now(UTC))

    resp = client.get(f"/auth/callback?code=abc&state={state}", follow_redirects=False)
    assert resp.status_code == 302
    assert resp.headers["location"] == "/"
    assert COOKIE_NAME in resp.cookies

    # The cookie the client now holds unlocks the browsing plane.
    assert client.get("/api/transcripts").status_code == 200
    me = client.get("/api/me")
    assert me.status_code == 200
    assert me.json() == {"userId": "pippijn", "displayName": "Pippijn"}


def test_callback_refuses_a_user_off_the_allowlist(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    cfg = _cfg({"pippijn"})
    client = TestClient(_app(cfg))
    monkeypatch.setattr(webauth, "exchange_code", lambda _cfg, _code: "access-tok")
    monkeypatch.setattr(
        webauth, "fetch_userinfo", lambda _cfg, _tok: Session("eve", "Eve")
    )
    state = make_state(cfg, "/", datetime.now(UTC))
    resp = client.get(f"/auth/callback?code=abc&state={state}", follow_redirects=False)
    assert resp.status_code == 403
    assert COOKIE_NAME not in resp.cookies


def test_gate_refuses_a_valid_session_for_a_non_allowlisted_user() -> None:
    # A correctly-signed cookie for someone not on the allowlist is still 403 at the
    # gate, not just at callback time.
    cfg = _cfg({"pippijn"})
    client = TestClient(_app(cfg))
    client.cookies.set(
        COOKIE_NAME, make_session_cookie(cfg, Session("eve", "Eve"), datetime.now(UTC))
    )
    assert client.get("/api/transcripts").status_code == 403


def test_register_is_inert_without_config() -> None:
    app = FastAPI()

    @app.get("/api/transcripts")
    def _transcripts() -> dict[str, str]:
        return {"ok": "open"}

    assert register_web_auth(app, None) is False
    # No gate: the browsing route answers without any cookie.
    assert TestClient(app).get("/api/transcripts").status_code == 200
