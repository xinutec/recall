from __future__ import annotations

import threading
import time
from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest
from fastapi import Request
from fastapi.testclient import TestClient

from conftest import make_flac, make_mp3
from recall import (
    api,
    api_capture,
    capture_control,
    loudness,
)
from recall.ids import AudioSegmentId, TranscriptId
from recall.liveness import Evidence
from recall.sources import AudioSource, SourceKind
from recall.store import Store, TranscriptSegment
from recall.timeline import Segment
from recall.vad import SpeechRegion

BASE = datetime(2026, 6, 13, 12, 0, 0, tzinfo=UTC)


def _capture_request(
    *, cookie: str | None = None, client_host: str = "10.100.0.5"
) -> Request:
    """A minimal real Request for the capture-control endpoints — enough for
    request_origin to read method/path/cookies/headers/client (#1347)."""
    headers: list[tuple[bytes, bytes]] = []
    if cookie is not None:
        headers.append((b"cookie", f"recall_session={cookie}".encode()))
    return Request(
        {
            "type": "http",
            "method": "POST",
            "path": "/api/capture/pause",
            "headers": headers,
            "query_string": b"",
            "scheme": "http",
            "server": ("testserver", 80),
            "client": (client_host, 54321),
        }
    )


def _seg(asr_model: str, provenance: str | None = None) -> TranscriptSegment:
    return TranscriptSegment(
        id=TranscriptId(1),
        audio_segment_id=AudioSegmentId(1),
        start=BASE,
        end=BASE + timedelta(seconds=2),
        text="x",
        language="en",
        language_confidence=0.9,
        asr_confidence=0.9,
        asr_model=asr_model,
        speaker_label=None,
        speaker_id=None,
        superseded_by=None,
        provenance=provenance,
    )


def _seed_candidates(root: Path, count: int) -> Store:
    """A DB with `count` labelling candidates (machine turns, mid confidence)."""
    flac = root / "usb-20260613T120000.flac"
    make_flac(flac, 30.0)
    store = Store.open(root / "recall.sqlite")
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    audio_id = store.add_audio_segment(
        Segment(
            source_id="usb",
            sequence=0,
            start=BASE,
            end=BASE + timedelta(seconds=30),
            path=str(flac),
            sample_rate=48000,
            channels=1,
        )
    )
    for i in range(count):
        # 2.5s / 4 words: clear of the queue's backchannel filter.
        store.add_transcript_segment(
            audio_segment_id=audio_id,
            start=BASE + timedelta(seconds=i),
            end=BASE + timedelta(seconds=i + 2.5),
            text=f"this is turn {i}",
            asr_model="whisper",
            language="nl",
            asr_confidence=0.5,
        )
    return store


def test_backfill_loudness_fills_the_cache_offline(tmp_path: Path) -> None:
    """The offline backfill measures loudness for unmeasured turns and persists it,
    so the request path has something to rank by. The work-list then drains.
    """
    store = _seed_candidates(tmp_path, count=5)

    assert len(store.segments_missing_loudness()) == 5
    measured = loudness.backfill_loudness(store)
    assert measured == 5
    assert store.segments_missing_loudness() == []
    store.close()


def test_sources_liveness_local_needs_a_measured_marker(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # "Active" means measured recording, never just process state: every source
    # reads from its .alive marker — the ingest refreshes a phone's on real signal,
    # the capture watchdog refreshes the mic's on real closed segments. A loaded,
    # unpaused capture agent with no measured proof yet is NOT active (that green
    # dot is how speech got spoken into a startup dead-window).
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    store = Store.open(tmp_path / "recall.sqlite")
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    store.register_source(
        AudioSource(id="pixel9", name="Pixel 9", kind=SourceKind.TCP_PCM, spec="")
    )
    # An uploaded recording (a meeting) is a source too, but NOT a live device — it
    # must not appear in the fleet view.
    store.add_source(
        AudioSource(id="meeting-x", name="Meeting", kind=SourceKind.UPLOAD, spec="")
    )
    store.close()
    monkeypatch.setattr("recall.capture_control.capture_running", lambda: True)
    paused = {"v": False}
    monkeypatch.setattr(
        "recall.capture_control.is_paused", lambda root, now: paused["v"]
    )

    before = {
        s["id"]: s["active"]
        for s in TestClient(api.app).get("/api/sources").json()["items"]
    }
    assert before["usb"] is False  # running, but no measured proof yet
    assert before["pixel9"] is False  # no live connection yet
    assert "meeting-x" not in before  # uploads aren't devices

    # fresh markers (real signal measured) → live
    for source_id in ("usb", "pixel9"):
        (tmp_path / source_id).mkdir()
        (tmp_path / source_id / ".alive").touch()
    after = {
        s["id"]: s["active"]
        for s in TestClient(api.app).get("/api/sources").json()["items"]
    }
    assert after["usb"] is True
    assert after["pixel9"] is True

    # a pause reads idle at once for the mic — its 75s marker window must not
    # keep the dot green after recording stopped
    paused["v"] = True
    stopped = {
        s["id"]: s["active"]
        for s in TestClient(api.app).get("/api/sources").json()["items"]
    }
    assert stopped["usb"] is False


def test_sources_liveness_on_the_fleet_uses_the_macs_report(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # On Isis there is no local capture agent and no phone sockets — the .alive markers
    # live on the Mac. So liveness must come from the Mac's mirror report, not local
    # files (the bug: /api/sources read host-local state Isis can't see, and showed
    # every mic dead). Every source reads from its reported .alive freshness; the mic
    # is additionally gated on the reported running state.
    monkeypatch.setenv("RECALL_ROLE", "fleet")
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    store = Store.open(tmp_path / "recall.sqlite")
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    store.register_source(
        AudioSource(id="pixel9", name="Pixel 9", kind=SourceKind.TCP_PCM, spec="")
    )

    now = datetime.now(UTC)
    # Reported running but no measured liveness shipped: nothing reads live — a
    # running agent is not proof of recording.
    capture_control.record_reported(store, running=True, paused_until=None, now=now)
    store.close()
    empty = {
        s["id"]: s["active"]
        for s in TestClient(api.app).get("/api/sources").json()["items"]
    }
    assert empty["usb"] is False
    assert empty["pixel9"] is False

    # the Mac's next report ships both sources' fresh .alive times (the mirror's
    # gather) → live on the fleet too
    store = Store.open(tmp_path / "recall.sqlite")
    capture_control.record_reported(
        store,
        running=True,
        paused_until=None,
        now=now,
        source_liveness={"pixel9": now.isoformat(), "usb": now.isoformat()},
    )
    store.close()
    live = {
        s["id"]: s["active"]
        for s in TestClient(api.app).get("/api/sources").json()["items"]
    }
    assert live["pixel9"] is True
    assert live["usb"] is True

    # when the Mac reports capture paused, the mic reads idle at once — even though
    # its marker (wide 75s+ window) is still fresh
    store = Store.open(tmp_path / "recall.sqlite")
    capture_control.record_reported(
        store,
        running=False,
        paused_until=(now + timedelta(hours=1)).isoformat(),
        now=now,
        source_liveness={"usb": now.isoformat()},
    )
    store.close()
    paused = {
        s["id"]: s["active"]
        for s in TestClient(api.app).get("/api/sources").json()["items"]
    }
    assert paused["usb"] is False


def test_fleet_capture_state_separates_desired_from_confirmed(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # The UI flap (seen live 2026-07-16): a pause POST answered with *intent*
    # ("paused") while the next poll answered with the Mac's *report* ("running"),
    # so the app claimed recording resumed for a beat. The API must serve BOTH
    # truths so clients can render "Pausing…" instead of flapping.
    monkeypatch.setenv("RECALL_ROLE", "fleet")
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    now = datetime.now(UTC)
    store = Store.open(tmp_path / "recall.sqlite")
    capture_control.record_reported(store, running=True, paused_until=None, now=now)
    store.close()

    # Settled: desired running, Mac confirms running.
    settled = api_capture.capture_status()
    assert settled["running"] is True
    assert settled["desiredRunning"] is True
    assert settled["settled"] is True
    assert settled["micReachable"] is True

    # Press pause: desired flips NOW; the Mac hasn't applied yet, so the state is
    # transitioning — confirmed still says running, and nothing here contradicts a
    # later poll (this exact shape is what the next poll returns too).
    pausing = api_capture.capture_pause(_capture_request())
    assert pausing["desiredRunning"] is False
    assert pausing["desiredPausedUntil"] is not None
    assert pausing["running"] is True  # the mic's last confirmed word
    assert pausing["settled"] is False

    # The Mac applies + reports the pause: settled again.
    store = Store.open(tmp_path / "recall.sqlite")
    capture_control.record_reported(
        store,
        running=False,
        paused_until=pausing["desiredPausedUntil"],
        now=datetime.now(UTC),
    )
    store.close()
    confirmed = api_capture.capture_status()
    assert confirmed["running"] is False
    assert confirmed["settled"] is True

    # A Mac that stops reporting: unreachable, never settled — the UI says so
    # instead of presenting intent as fact.
    store = Store.open(tmp_path / "recall.sqlite")
    capture_control.record_reported(
        store,
        running=False,
        paused_until=pausing["desiredPausedUntil"],
        now=datetime.now(UTC) - timedelta(minutes=5),
    )
    store.close()
    unreachable = api_capture.capture_status()
    assert unreachable["micReachable"] is False
    assert unreachable["settled"] is False
    assert unreachable["running"] is False  # falls back to desired


def test_local_capture_state_is_always_settled(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # Locally (dev) the pause file IS the actuation — desired and confirmed are the
    # same thing, so the state is settled by construction.
    monkeypatch.delenv("RECALL_ROLE", raising=False)
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    monkeypatch.setattr("recall.capture_control.capture_running", lambda: True)
    state = api_capture.capture_pause(_capture_request())
    assert state["running"] is False
    assert state["desiredRunning"] is False
    assert state["settled"] is True
    assert state["micReachable"] is True


def test_a_local_pause_records_who_asked(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # #1347: capture-control is login-free on the recording plane, so the durable
    # record must at least carry the peer that asked — enough to answer "was that
    # pause mine?". Auth is off locally, so origin is the peer address.
    monkeypatch.delenv("RECALL_ROLE", raising=False)
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    monkeypatch.setattr("recall.capture_control.capture_running", lambda: True)
    api_capture.capture_pause(_capture_request(client_host="192.168.1.42"))

    store = Store.open(tmp_path / "recall.sqlite")
    events = store.capture_events_since(
        datetime.now(UTC) - timedelta(minutes=1),
        kinds=(capture_control.CaptureEventKind.CONTROL_REQUEST,),
    )
    store.close()
    assert len(events) == 1
    assert events[0].detail is not None
    assert "pause" in events[0].detail
    assert "192.168.1.42" in events[0].detail


def _sessions_via_store(tmp_path: Path) -> list[dict[str, object]]:
    """What the sessions list would show, read from the store.

    ⚠ Deliberately not `GET /api/sessions`: that route is recalld's now. These
    tests cover the upload and the delete, which are still Python's, so they must
    verify through the database both halves share rather than through a route
    this process no longer serves.
    """
    store = Store.open(tmp_path / "recall.sqlite")
    try:
        return [
            {"id": sid, "title": name, "turnCount": turns}
            for sid, name, _start, _end, turns, _speakers in store.session_summaries()
        ]
    finally:
        store.close()


def _upload_meeting(
    client: TestClient, tmp_path: Path, *, title: str = "", start: str = ""
) -> dict[str, object]:
    """POST a real mp3 to /api/sessions and return the created session."""
    src = tmp_path / "hospital.mp3"
    make_mp3(src, 4.0)
    data: dict[str, str] = {}
    if title:
        data["title"] = title
    if start:
        data["start"] = start
    with src.open("rb") as fh:
        r = client.post(
            "/api/sessions",
            files={"audio": ("hospital.mp3", fh, "audio/mpeg")},
            data=data,
        )
    assert r.status_code == 200, r.text
    created: dict[str, object] = r.json()
    return created


def _usb_store(tmp_path: Path) -> Store:
    store = Store.open(tmp_path / "recall.sqlite")
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    return store


def _seed_today(tmp_path: Path, texts: list[str]) -> None:
    """Turns landing today (the endpoint anchors on the real UTC clock)."""
    now = datetime.now(UTC).replace(minute=0, second=0, microsecond=0)
    store = Store.open(tmp_path / "recall.sqlite")
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    audio_id = store.add_audio_segment(
        Segment(
            source_id="usb",
            sequence=0,
            start=now,
            end=now + timedelta(minutes=30),
            path="x",
            sample_rate=48000,
            channels=1,
        )
    )
    for i, text in enumerate(texts):
        store.add_transcript_segment(
            audio_segment_id=audio_id,
            start=now + timedelta(seconds=i * 10),
            end=now + timedelta(seconds=i * 10 + 5),
            text=text,
            asr_model="whisper",
            speaker_label="Alice",
        )
    store.close()


def _deaf(monkeypatch: pytest.MonkeyPatch) -> None:
    """A speech detector that hears nothing — Silero is a real model, these are not real
    files, and what is under test here is the plumbing, not the detector."""
    monkeypatch.setattr(
        "recall.analyse.silero_speech_regions", lambda _p: list[SpeechRegion]()
    )


def _await_scan(client: TestClient, timeout_s: float = 10.0) -> dict[str, object]:
    """Start the background scan and wait for it to finish, as the page's poll does.

    The scan also runs the speech detector over its candidates (recall.analyse); tests
    stub that out with `_deaf` — Silero is a real model and these are not real files.
    """
    scan: dict[str, object] = client.post("/api/quiet/scan").json()
    deadline = time.monotonic() + timeout_s
    while scan["running"] and time.monotonic() < deadline:
        time.sleep(0.01)
        scan = client.get("/api/quiet/scan").json()
    assert not scan["running"], f"scan did not finish in {timeout_s}s: {scan}"
    return scan


def test_capture_pause_on_the_fleet_records_intent_not_the_local_file(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # On Isis there is no capture agent, so a local pause file would actuate nothing —
    # the fleet records intent instead, and the Mac mirrors it onto the real mic.
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    monkeypatch.setenv("RECALL_ROLE", "fleet")
    Store.open(tmp_path / "recall.sqlite").close()
    client = TestClient(api.app)

    paused = client.post("/api/capture/pause").json()
    assert paused["running"] is False
    assert paused["pausedUntil"]
    assert not (tmp_path / "capture_paused_until").exists()  # intent, not a local file

    # With no Mac report yet, status falls back to the intent it holds.
    assert client.get("/api/capture").json()["running"] is False

    assert client.post("/api/capture/resume").json()["running"] is True
    assert client.get("/api/capture").json()["running"] is True


def test_capture_status_on_the_fleet_shows_the_macs_reported_reality(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # The fleet asked for a pause, but the Mac reports it is still recording (it hasn't
    # applied it yet). Status must show reality, not the wish — a pause you can't
    # confirm is worthless.
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    monkeypatch.setenv("RECALL_ROLE", "fleet")
    Store.open(tmp_path / "recall.sqlite").close()
    client = TestClient(api.app)

    client.post("/api/capture/pause")
    store = Store.open(tmp_path / "recall.sqlite")
    capture_control.record_reported(
        store, running=True, paused_until=None, now=datetime.now(UTC)
    )
    store.close()

    assert client.get("/api/capture").json()["running"] is True


def test_capture_long_poll_wakes_on_a_press(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # The latency fix: a GET hanging on ?wait&known is woken by a pause press in
    # ~RTT — the press propagates to every watching client without a poll interval.
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    monkeypatch.setenv("RECALL_ROLE", "fleet")
    Store.open(tmp_path / "recall.sqlite").close()
    client = TestClient(api.app)

    first = client.get("/api/capture").json()
    token = first["stateToken"]
    assert token  # every response carries the fingerprint a long-poll echoes back
    # the fingerprint is stable while nothing changes…
    assert client.get("/api/capture").json()["stateToken"] == token

    results: list[dict[str, object]] = []

    def hang() -> None:
        results.append(
            client.get("/api/capture", params={"wait": 10, "known": token}).json()
        )

    waiter = threading.Thread(target=hang)
    waiter.start()
    time.sleep(0.3)  # let the GET reach its hang
    assert waiter.is_alive()  # …so the request is actually held, not answered
    client.post("/api/capture/pause")
    waiter.join(timeout=5.0)
    assert not waiter.is_alive()  # the press woke it, not the 10s wait
    assert results[0]["desiredRunning"] is False
    assert results[0]["stateToken"] != token


def test_capture_long_poll_times_out_quietly_when_nothing_changes(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    monkeypatch.setenv("RECALL_ROLE", "fleet")
    Store.open(tmp_path / "recall.sqlite").close()
    client = TestClient(api.app)

    token = client.get("/api/capture").json()["stateToken"]
    started = time.monotonic()
    state = client.get("/api/capture", params={"wait": 0.4, "known": token}).json()
    assert time.monotonic() - started >= 0.35  # held for the wait…
    assert state["stateToken"] == token  # …and returned the unchanged state


def test_sources_liveness_sees_a_store_and_forward_recorder(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # geb streams to nothing since the C3 cutover, so no .alive marker of its is
    # ever refreshed again and the panel showed it off while it recorded (#1428).
    # Its delivered segments are the evidence that does exist.
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    monkeypatch.setattr("recall.capture_control.capture_running", lambda: True)
    monkeypatch.setattr("recall.capture_control.is_paused", lambda root, now: False)
    store = Store.open(tmp_path / "recall.sqlite")
    store.register_source(
        AudioSource(id="geb", name="geb", kind=SourceKind.TCP_PCM, spec="")
    )
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    store.close()

    now = datetime.now(UTC)
    fresh = Evidence(now - timedelta(minutes=1), now - timedelta(minutes=1))
    monkeypatch.setattr("recall.api_devices.delivered_liveness", lambda: {"geb": fresh})
    items = {
        s["id"]: s for s in TestClient(api.app).get("/api/sources").json()["items"]
    }
    assert items["geb"]["active"] is True
    # The panel shows the evidence that proved it, not a frozen marker.
    assert items["geb"]["lastActive"] is not None
    # usb has no delivery and no marker: unchanged, still idle.
    assert items["usb"]["active"] is False


def test_a_paused_mic_is_not_resurrected_by_its_own_delivered_segments(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # The pause must read idle AT ONCE. Delivered evidence is up to a segment
    # old, so audio captured just before the pause must not hold the dot green.
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    monkeypatch.setattr("recall.capture_control.capture_running", lambda: True)
    monkeypatch.setattr("recall.capture_control.is_paused", lambda root, now: True)
    store = Store.open(tmp_path / "recall.sqlite")
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    store.close()

    now = datetime.now(UTC)
    fresh = Evidence(now - timedelta(seconds=30), now - timedelta(seconds=30))
    monkeypatch.setattr("recall.api_devices.delivered_liveness", lambda: {"usb": fresh})
    items = {
        s["id"]: s for s in TestClient(api.app).get("/api/sources").json()["items"]
    }
    assert items["usb"]["active"] is False


def test_a_household_pause_silences_delivered_evidence_too(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # A pause stops every recorder, so segments captured just before it must not
    # keep any dot green for the delivered window — the same "idle at once"
    # promise the mic already had, extended to the store-and-forward proof.
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    monkeypatch.setattr("recall.capture_control.capture_running", lambda: True)
    monkeypatch.setattr("recall.capture_control.is_paused", lambda root, now: True)
    store = Store.open(tmp_path / "recall.sqlite")
    store.register_source(
        AudioSource(id="geb", name="geb", kind=SourceKind.TCP_PCM, spec="")
    )
    store.close()

    now = datetime.now(UTC)
    fresh = Evidence(now - timedelta(seconds=30), now - timedelta(seconds=30))
    monkeypatch.setattr("recall.api_devices.delivered_liveness", lambda: {"geb": fresh})
    items = {
        s["id"]: s for s in TestClient(api.app).get("/api/sources").json()["items"]
    }
    assert items["geb"]["active"] is False
