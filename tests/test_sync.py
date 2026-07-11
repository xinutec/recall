"""Mac→fleet sync: the auth gate and the job-poll loop.

The security-critical parts are tested: a request without the right bearer token is
refused, the endpoints are absent unless a token is configured, and a real poll/done
round-trip goes through the store. The pure auth check covers the timing-safe compare
and the "not enabled" vs "wrong token" distinction."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest
from fastapi import FastAPI, HTTPException
from fastapi.testclient import TestClient

from recall.sources import AudioSource, SourceKind
from recall.store import Store
from recall.sync import (
    SYNC_TOKEN_ENV,
    SyncClient,
    bearer,
    check_token,
    register_sync_routes,
)

BASE = datetime(2026, 7, 11, 12, 0, 0, tzinfo=UTC)


def test_bearer_parses_only_the_bearer_scheme() -> None:
    assert bearer("Bearer abc") == "abc"
    assert bearer("abc") is None
    assert bearer("Basic abc") is None
    assert bearer(None) is None


def test_check_token_503_when_the_server_has_no_token() -> None:
    # the split is off — never silently accept, and don't look like a bad-token 401
    with pytest.raises(HTTPException) as exc:
        check_token("anything", None)
    assert exc.value.status_code == 503


def test_check_token_401_when_missing_or_wrong() -> None:
    for presented in (None, "", "wrong"):
        with pytest.raises(HTTPException) as exc:
            check_token(presented, "secret")
        assert exc.value.status_code == 401


def test_check_token_passes_on_the_right_token() -> None:
    check_token("secret", "secret")  # no raise


def test_routes_are_absent_without_a_configured_token(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.delenv(SYNC_TOKEN_ENV, raising=False)
    app = FastAPI()
    assert register_sync_routes(app, Store.memory, tmp_path) is False
    assert TestClient(app).get("/sync/jobs").status_code == 404  # never registered


def _seed(path: Path) -> None:
    store = Store.open(path)
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    store.add_refine_request("usb", BASE, BASE + timedelta(minutes=5))
    store.close()


def test_poll_and_done_roundtrip(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.setenv(SYNC_TOKEN_ENV, "secret")
    db = tmp_path / "recall.sqlite"
    _seed(db)

    app = FastAPI()
    assert register_sync_routes(app, lambda: Store.open(db), tmp_path) is True
    client = TestClient(app)

    # the gate: no token, no jobs
    assert client.get("/sync/jobs").status_code == 401

    auth = {"Authorization": "Bearer secret"}
    jobs = client.get("/sync/jobs", headers=auth).json()
    assert len(jobs) == 1
    assert jobs[0]["type"] == "refine"
    assert jobs[0]["source"] == "usb"

    # finishing the job removes it from the queue
    assert (
        client.post(f"/sync/jobs/{jobs[0]['id']}/done", headers=auth).status_code == 200
    )
    assert client.get("/sync/jobs", headers=auth).json() == []


def test_sync_client_polls_and_marks_done_over_the_transport(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    # Drive the real SyncClient against the app's test transport — proving the Mac-side
    # client and the fleet-side routes agree on the wire contract.
    monkeypatch.setenv(SYNC_TOKEN_ENV, "secret")
    db = tmp_path / "recall.sqlite"
    _seed(db)
    app = FastAPI()
    register_sync_routes(app, lambda: Store.open(db), tmp_path)

    with TestClient(app) as transport:
        client = SyncClient("http://fleet", "secret", client=transport)
        jobs = client.poll_jobs()
        assert [j.type for j in jobs] == ["refine"]
        client.mark_done(jobs[0].id)
        assert client.poll_jobs() == []


def test_audio_push_stores_once_and_is_idempotent(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.setenv(SYNC_TOKEN_ENV, "secret")
    archive = tmp_path / "archive"
    app = FastAPI()
    register_sync_routes(app, Store.memory, archive)

    clip = tmp_path / "clip.opus"
    clip.write_bytes(b"opus-bytes")

    with TestClient(app) as transport:
        client = SyncClient("http://fleet", "secret", client=transport)
        assert client.push_audio("usb", "seg-0001.opus", clip) is True  # newly stored
        landed = archive / "usb" / "seg-0001.opus"
        assert landed.read_bytes() == b"opus-bytes"
        # re-push of the immutable archive is a no-op, not an overwrite
        assert client.push_audio("usb", "seg-0001.opus", clip) is False


def test_audio_push_requires_the_token(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.setenv(SYNC_TOKEN_ENV, "secret")
    app = FastAPI()
    register_sync_routes(app, Store.memory, tmp_path)
    resp = TestClient(app).post(
        "/sync/audio",
        data={"source": "usb", "name": "x.opus"},
        files={"file": ("x", b"z")},
    )
    assert resp.status_code == 401


def test_audio_push_rejects_path_traversal(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.setenv(SYNC_TOKEN_ENV, "secret")
    app = FastAPI()
    register_sync_routes(app, Store.memory, tmp_path)
    auth = {"Authorization": "Bearer secret"}
    resp = TestClient(app).post(
        "/sync/audio",
        data={"source": "../escape", "name": "x.opus"},
        files={"file": ("x", b"z")},
        headers=auth,
    )
    assert resp.status_code == 400
