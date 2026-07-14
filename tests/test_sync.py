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
from httpx import HTTPStatusError

from recall.ids import AudioSegmentId
from recall.sources import AudioSource, SourceKind
from recall.store import Store
from recall.sync import (
    SYNC_TOKEN_ENV,
    SegmentIn,
    SummaryIn,
    SyncClient,
    TurnIn,
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


def _segment(
    source: str = "usb", n_turns: int = 2, path: str | None = None
) -> SegmentIn:
    return SegmentIn(
        source_id=source,
        source_name="usb",
        kind="coreaudio",
        path=path if path is not None else f"/archive/{source}/seg.opus",
        start=BASE.isoformat(),
        end=(BASE + timedelta(seconds=30)).isoformat(),
        sample_rate=48000,
        channels=1,
        turns=[
            TurnIn(
                start=(BASE + timedelta(seconds=i)).isoformat(),
                end=(BASE + timedelta(seconds=i + 1)).isoformat(),
                text=f"turn {i}",
                asr_model="mlx-community/whisper-large-v3-turbo",
            )
            for i in range(n_turns)
        ],
    )


def test_segment_push_writes_turns_then_is_idempotent(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.setenv(SYNC_TOKEN_ENV, "secret")
    db = tmp_path / "recall.sqlite"
    app = FastAPI()
    register_sync_routes(app, lambda: Store.open(db), tmp_path)

    with TestClient(app) as transport:
        client = SyncClient("http://fleet", "secret", client=transport)
        first = client.push_segment(_segment(n_turns=2))
        assert first.turns_written == 2
        # re-push of the same segment is a no-op (first-write-wins), not a duplicate
        again = client.push_segment(_segment(n_turns=2))
        assert again.turns_written == 0
        assert again.audio_segment_id == first.audio_segment_id

    # the turns really landed in the fleet's store
    store = Store.open(db)
    turns = store.visible_machine_turns_for_audio(first.audio_segment_id)
    store.close()
    assert sorted(t.text for t in turns) == ["turn 0", "turn 1"]


def test_segment_repush_supersedes_the_old_machine_turns(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    # A later re-derivation (worker → refine) pushes a different turn set; it supersedes
    # the old machine turns rather than duplicating or being ignored.
    monkeypatch.setenv(SYNC_TOKEN_ENV, "secret")
    db = tmp_path / "recall.sqlite"
    app = FastAPI()
    register_sync_routes(app, lambda: Store.open(db), tmp_path)

    with TestClient(app) as transport:
        client = SyncClient("http://fleet", "secret", client=transport)
        client.push_segment(_segment(n_turns=2))  # worker turns
        refined = _segment(n_turns=2)
        refined.turns[0].text = "diarized turn A"
        refined.turns[1].text = "diarized turn B"
        refined.turns[0].asr_model = "adapter"
        refined.turns[1].asr_model = "adapter"
        result = client.push_segment(refined)
        assert result.turns_written == 2

    store = Store.open(db)
    visible = store.visible_machine_turns_for_audio(result.audio_segment_id)
    store.close()
    # only the refined turns are visible; the worker turns were superseded (hidden)
    assert sorted(t.text for t in visible) == ["diarized turn A", "diarized turn B"]


def test_summary_push_upserts_by_day(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.setenv(SYNC_TOKEN_ENV, "secret")
    db = tmp_path / "recall.sqlite"
    app = FastAPI()
    register_sync_routes(app, lambda: Store.open(db), tmp_path)

    with TestClient(app) as transport:
        client = SyncClient("http://fleet", "secret", client=transport)
        client.push_summary(
            SummaryIn(day="2026-07-11", text="a quiet day", model="qwen")
        )
        # re-push replaces (upsert by day) — not a duplicate
        client.push_summary(SummaryIn(day="2026-07-11", text="revised", model="qwen"))

    store = Store.open(db)
    got = store.get_day_summary("2026-07-11")
    store.close()
    assert got == "revised"


def test_segment_push_rejects_a_bad_source_kind(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.setenv(SYNC_TOKEN_ENV, "secret")
    app = FastAPI()
    register_sync_routes(app, lambda: Store.open(tmp_path / "r.sqlite"), tmp_path)
    seg = _segment()
    seg.kind = "not-a-kind"
    resp = TestClient(app).post(
        "/sync/segments",
        json=seg.model_dump(),
        headers={"Authorization": "Bearer secret"},
    )
    assert resp.status_code == 400


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


def test_the_fleet_rehomes_the_path_into_its_own_archive(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """The bug that would have made every recording on the fleet unplayable.

    A segment's `path` is absolute and belongs to the machine that recorded it —
    `/Volumes/Backup/recall/usb/usb-20260613T172550.opus` on the Mac. The push stored it
    verbatim, while the audio blob it accompanies lands under the fleet's OWN root
    (`/data/usb/...`, see the /sync/audio route). The fleet's database therefore
    described files on a filesystem it cannot see: transcripts would look perfect and
    every play button would fail, for ever, silently. Found on the real Isis
    deployment — three test segments, three paths pointing at the Mac.

    The fleet owns its archive layout, so the fleet decides where its files are. The
    sender's directories are none of its business; only the filename survives.
    """
    monkeypatch.setenv(SYNC_TOKEN_ENV, "secret")
    db = tmp_path / "recall.sqlite"
    app = FastAPI()
    register_sync_routes(app, lambda: Store.open(db), tmp_path)

    with TestClient(app) as transport:
        client = SyncClient("http://fleet", "secret", client=transport)
        pushed = client.push_segment(
            _segment(source="usb"),
        )

    store = Store.open(db)
    segment = store.audio_segment(AudioSegmentId(pushed.audio_segment_id))
    store.close()
    assert segment is not None
    # Where the /sync/audio route actually writes the blob — not where the Mac kept it.
    assert segment.path == str(tmp_path / "usb" / "seg.opus")


def test_a_pushed_path_can_never_escape_the_fleet_archive(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """The Mac is authenticated, but a stolen token must not become a file write
    anywhere on the fleet. Re-homing makes that structural rather than vigilant: only
    the basename survives, so a hostile path cannot point outside the archive even in
    principle — there is no directory left in it to point with.
    """
    monkeypatch.setenv(SYNC_TOKEN_ENV, "secret")
    db = tmp_path / "recall.sqlite"
    app = FastAPI()
    register_sync_routes(app, lambda: Store.open(db), tmp_path)

    with TestClient(app) as transport:
        client = SyncClient("http://fleet", "secret", client=transport)
        pushed = client.push_segment(
            _segment(source="usb", path="/etc/../../root/.ssh/authorized_keys")
        )

    store = Store.open(db)
    segment = store.audio_segment(AudioSegmentId(pushed.audio_segment_id))
    store.close()
    assert segment is not None
    # Contained: inside the fleet's archive, under the pushing source, basename only.
    assert Path(segment.path).parent == tmp_path / "usb"
    assert Path(segment.path).is_relative_to(tmp_path)


def test_a_filename_that_is_not_a_filename_is_refused(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    # A basename of ".." is not a name at all. Rejected outright rather than sanitised
    # into something plausible — a push the fleet cannot make sense of is an error.
    monkeypatch.setenv(SYNC_TOKEN_ENV, "secret")
    app = FastAPI()
    register_sync_routes(app, lambda: Store.open(tmp_path / "recall.sqlite"), tmp_path)

    with TestClient(app) as transport:
        client = SyncClient("http://fleet", "secret", client=transport)
        with pytest.raises(HTTPStatusError):
            client.push_segment(_segment(source="usb", path="/archive/usb/.."))
