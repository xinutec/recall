from __future__ import annotations

import threading
import time
from dataclasses import replace
from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest
from fastapi import HTTPException, Request
from fastapi.testclient import TestClient

from conftest import make_flac, make_mp3
from recall import api, capture_control, loudness
from recall.abcompare import CorrectionScore, Report, SegmentDiff, render_json
from recall.api import _precise, _tier, clip_window
from recall.api_models import VoiceNameIn
from recall.asr import Word
from recall.envelope import DEFAULT_EVENT_DB, Measurement
from recall.ids import AudioSegmentId, TranscriptId
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


def test_tier_classifies_by_model_and_provenance() -> None:
    assert _tier(_seg("human")) == "corrected"
    assert _tier(_seg("live")) == "live"
    assert _tier(_seg("mlx-whisper", "diarized (mlx-whisper)")) == "diarized"
    assert _tier(_seg("mlx-whisper", "mlx-whisper")) == "transcribed"


def test_precise_plays_tight_for_diarized_or_word_timed_turns() -> None:
    # A diarized turn and any turn carrying word timings (e.g. a span-assign split) are
    # precise cutouts → played tight. A plain transcribed phrase gets the wide window.
    diarized = _seg("mlx-whisper", "diarized (mlx-whisper)")
    basic = _seg("mlx-whisper", "mlx-whisper")
    split = replace(
        basic, provenance="split of #5", word_timings=(Word(0.0, 0.5, "a", 1.0),)
    )
    assert _precise(diarized) is True
    assert _precise(split) is True  # word timings → tight, exact
    assert _precise(basic) is False  # rough phrase → wide context window


def test_clip_window_tight_for_a_cutout() -> None:
    # A diarized cutout is played tight: just the span + a small safety pad.
    assert clip_window(10.0, 12.0, pad=0.2, minimum=0.0) == (9.8, 12.2)


def test_clip_window_pads_a_long_phrase() -> None:
    # A comfortably long phrase just gets the context pad on each side.
    assert clip_window(10.0, 30.0, pad=1.5, minimum=5.0) == (8.5, 31.5)


def test_clip_window_expands_short_phrase_to_minimum() -> None:
    # A 1s phrase padded to 4s is still under the floor → centred 5s window.
    assert clip_window(10.0, 11.0, pad=1.5, minimum=5.0) == (8.0, 13.0)


def test_clip_window_clamps_start_at_zero() -> None:
    start, end = clip_window(0.2, 0.5, pad=1.5, minimum=5.0)
    assert start == 0.0
    assert end > 0.0


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


def test_train_ranks_substantial_novel_turns_first(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Best-first order promotes a long, not-yet-labelled turn over a short one
    and over a phrase already in the corpus — value, not just loudness."""
    flac = tmp_path / "usb-20260613T120000.flac"
    make_flac(flac, 30.0)
    store = Store.open(tmp_path / "recall.sqlite")
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

    def turn(text: str, at: float, dur: float) -> int:
        return store.add_transcript_segment(
            audio_segment_id=audio_id,
            start=BASE + timedelta(seconds=at),
            end=BASE + timedelta(seconds=at + dur),
            text=text,
            asr_model="whisper",
            language="nl",
            asr_confidence=0.5,
        )

    long_novel = turn("a full clear sentence worth learning", 0.0, 4.0)
    short = turn("ja", 6.0, 0.7)
    already = turn("okay that is fine", 8.0, 4.0)
    # all equally loud, so only the value score separates them
    for sid in (long_novel, short, already):
        store.set_loudness(sid, 0.1)
    # "okay that is fine" is already in the corpus → a repeat, deprioritised
    repeat_seg = turn("okay that is fine", 12.0, 1.0)
    store.add_correction(
        transcript_segment_id=repeat_seg,
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=12),
        end=BASE + timedelta(seconds=13),
        original_text="okay that is fine",
        corrected_text="okay that is fine",
        language="nl",
        created=BASE,
        speaker="Pippijn",
    )
    store.close()
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)

    items = api.train(limit=10)["items"]
    assert isinstance(items, list)
    texts = [i["text"] for i in items]
    assert texts[0] == "a full clear sentence worth learning"
    # the already-labelled phrase ranks below the long novel one
    assert texts.index("a full clear sentence worth learning") < texts.index(
        "okay that is fine"
    )


def test_train_request_path_does_not_decode_audio(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The /api/train request must be a cheap read: loudness is precomputed, never
    measured (sox decode) per-candidate on the request path. That synchronous
    decode loop made the endpoint take ~13s for 80 candidates and time out the
    phone. Guards the 'fast UX, offline precision' contract.
    """
    store = _seed_candidates(tmp_path, count=20)
    store.close()
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)

    calls = 0

    def _counting_speech_level(*_args: object, **_kwargs: object) -> float:
        nonlocal calls
        calls += 1
        return 0.0

    monkeypatch.setattr(loudness, "speech_level", _counting_speech_level)

    result = api.train(limit=40)

    items = result["items"]
    assert isinstance(items, list)
    assert items, "should still return the candidates"
    assert calls == 0, f"request path decoded audio (calls={calls}); must read cache"


def test_train_time_order_returns_turns_chronologically(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """order=time reads the window in sequence (oldest first), so a conversation
    can be followed as it happened rather than by loudness."""
    store = _seed_candidates(tmp_path, count=5)  # "this is turn 0..4", ascending
    store.close()
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)

    result = api.train(limit=40, order="time")
    items = result["items"]
    assert isinstance(items, list)
    assert [i["text"] for i in items] == [f"this is turn {i}" for i in range(5)]


def test_conversations_groups_turns_by_silence_gaps(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The /api/conversations endpoint windows recent turns and breaks them into
    conversations on silence, carrying each turn plus a summary for the card."""
    flac = tmp_path / "usb-20260613T120000.flac"
    make_flac(flac, 30.0)
    store = Store.open(tmp_path / "recall.sqlite")
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    audio_id = store.add_audio_segment(
        Segment(
            source_id="usb",
            sequence=0,
            start=BASE,
            end=BASE + timedelta(hours=1),
            path=str(flac),
            sample_rate=48000,
            channels=1,
        )
    )

    def turn(text: str, at: float, dur: float, speaker: str | None) -> None:
        store.add_transcript_segment(
            audio_segment_id=audio_id,
            start=BASE + timedelta(seconds=at),
            end=BASE + timedelta(seconds=at + dur),
            text=text,
            asr_model="whisper",
            language="nl",
            asr_confidence=0.9,
            speaker_label=speaker,
        )

    # Conversation 1: a tight exchange. Conversation 2: after a 10-minute silence.
    turn("Carol, is dit dringend?", 0, 2, "Carol")
    turn("Nee hoor.", 4, 2, "Alice")
    turn("Echt waar?", 700, 2, "Carol")
    store.close()
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)

    result = api.conversations(limit=200)
    items = result["items"]
    assert isinstance(items, list)
    assert len(items) == 2  # the 10-minute gap split them

    first = items[0]
    assert first["turnCount"] == 2
    assert first["speakers"] == ["Carol", "Alice"]
    assert first["preview"] == "Carol, is dit dringend?"
    # One mic, so each turn is its own moment (nothing to fold); the spine is the
    # turn, no alternates.
    assert [m["primary"][0]["text"] for m in first["moments"]] == [
        "Carol, is dit dringend?",
        "Nee hoor.",
    ]
    assert all(m["alternates"] == [] for m in first["moments"])
    assert items[1]["turnCount"] == 1


def test_folded_moment_shows_strongest_colocated_guess(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A folded moment keeps the cleaner *transcription* as the spine, but shows the
    most confident attribution among the co-located mics — the same speech, so the
    spine mic's weaker guess isn't the one displayed."""
    store = Store.open(tmp_path / "recall.sqlite")
    for sid in ("usb", "pixel9"):
        store.add_source(
            AudioSource(id=sid, name=sid, kind=SourceKind.COREAUDIO, spec="")
        )

    def add(sid: str, text: str, asr_conf: float) -> int:
        audio = store.add_audio_segment(
            Segment(
                source_id=sid,
                sequence=0,
                start=BASE,
                end=BASE + timedelta(seconds=5),
                path=f"{sid}.flac",
                sample_rate=48000,
                channels=1,
            )
        )
        return store.add_transcript_segment(
            audio_segment_id=audio,
            start=BASE,
            end=BASE + timedelta(seconds=3),
            text=text,
            asr_model="whisper",
            language="en",
            asr_confidence=asr_conf,
        )

    usb = add("usb", "air con usb", 0.9)  # cleaner transcription -> the spine
    pix = add("pixel9", "air con pixel", 0.5)  # weaker text, stronger voiceprint
    store.set_speaker_guess(usb, "Pippijn", 0.40)
    store.set_speaker_guess(pix, "Pippijn", 0.80)
    store.close()
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)

    moments = api.conversations(limit=200)["items"][0]["moments"]
    assert len(moments) == 1
    primary = moments[0]["primary"]
    assert [p["text"] for p in primary] == ["air con usb"]  # the cleaner-mic spine
    assert primary[0]["speaker"] == "Pippijn"
    # The spine mic guessed 0.40, but the co-located mic heard the same speech at 0.80
    # — that's the guess shown on the spine turn.
    assert primary[0]["speakerConfidence"] == 0.80
    assert moments[0]["alternates"][0]["speakerConfidence"] == 0.80


def test_transcript_exposes_confirmed_vs_guessed_speaker(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A human label is authoritative (confirmed, no probability); a machine turn
    shows its best auto guess with the match strength."""
    flac = tmp_path / "usb-20260613T120000.flac"
    make_flac(flac, 30.0)
    store = Store.open(tmp_path / "recall.sqlite")
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
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=1),
        text="confirmed",
        asr_model="human",
        speaker_label="Carol",
    )
    guessed = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=2),
        end=BASE + timedelta(seconds=3),
        text="guessed",
        asr_model="whisper",
        asr_confidence=0.5,
    )
    store.set_speaker_guess(guessed, "Alice", 0.31)
    store.close()
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)

    items = api.timeline(limit=10)["items"]
    assert isinstance(items, list)
    by_text = {i["text"]: i for i in items}

    c = by_text["confirmed"]
    assert c["speaker"] == "Carol"
    assert c["speakerConfirmed"] is True
    assert c["speakerConfidence"] is None  # confirmed: no probability shown

    g = by_text["guessed"]
    assert g["speaker"] == "Alice"
    assert g["speakerConfirmed"] is False
    assert g["speakerConfidence"] == 0.31  # the match strength


def test_suggest_reads_the_cached_guess_above_threshold(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The Train 'sounds like X' hint reads the cached guess (kept fresh by the
    worker), so it agrees with the timeline — and only pre-fills a confident one."""
    store = Store.open(tmp_path / "recall.sqlite")
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    audio_id = store.add_audio_segment(
        Segment(
            source_id="usb",
            sequence=0,
            start=BASE,
            end=BASE + timedelta(seconds=2),
            path="x.flac",
            sample_rate=48000,
            channels=1,
        )
    )
    confident = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=1),
        text="hi",
        asr_model="whisper",
    )
    store.set_speaker_guess(confident, "Alice", 0.82)  # clear leading likelihood
    weak = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=1),
        end=BASE + timedelta(seconds=2),
        text="ho",
        asr_model="whisper",
    )
    store.set_speaker_guess(weak, "Carol", 0.33)  # a toss-up — don't pre-fill
    store.close()
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)

    assert api.suggest(confident)["speaker"] == "Alice"
    assert api.suggest(weak)["speaker"] is None  # below the pre-fill bar


def test_conversations_gap_param_is_tunable(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A larger gap merges turns a smaller one would split (calibration knob)."""
    flac = tmp_path / "usb-20260613T120000.flac"
    make_flac(flac, 30.0)
    store = Store.open(tmp_path / "recall.sqlite")
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    audio_id = store.add_audio_segment(
        Segment(
            source_id="usb",
            sequence=0,
            start=BASE,
            end=BASE + timedelta(hours=1),
            path=str(flac),
            sample_rate=48000,
            channels=1,
        )
    )
    for at in (0.0, 100.0):  # two turns 100 seconds apart
        store.add_transcript_segment(
            audio_segment_id=audio_id,
            start=BASE + timedelta(seconds=at),
            end=BASE + timedelta(seconds=at + 1),
            text="x",
            asr_model="whisper",
            language="nl",
            asr_confidence=0.9,
        )
    store.close()
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)

    split = api.conversations(limit=10, gap=60.0)["items"]
    merged = api.conversations(limit=10, gap=200.0)["items"]
    assert isinstance(split, list)
    assert isinstance(merged, list)
    assert len(split) == 2
    assert len(merged) == 1


def test_backfill_loudness_fills_the_cache_offline(tmp_path: Path) -> None:
    """The offline backfill measures loudness for unmeasured turns and persists it,
    so the request path has something to rank by. The work-list then drains.
    """
    store = _seed_candidates(tmp_path, count=5)

    assert len(store.segments_missing_loudness()) == 5
    measured = loudness.backfill_loudness(store)
    assert measured == 5
    assert store.segments_missing_loudness() == []
    # Every turn now has a cached loudness (the 440Hz tone is audible → > 0).
    queued = store.training_queue(min_confidence=0.3, max_confidence=0.95, limit=40)
    assert all(s.loudness is not None for s in queued)
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

    before = {s["id"]: s["active"] for s in api.sources()["items"]}
    assert before["usb"] is False  # running, but no measured proof yet
    assert before["pixel9"] is False  # no live connection yet
    assert "meeting-x" not in before  # uploads aren't devices

    # fresh markers (real signal measured) → live
    for source_id in ("usb", "pixel9"):
        (tmp_path / source_id).mkdir()
        (tmp_path / source_id / ".alive").touch()
    after = {s["id"]: s["active"] for s in api.sources()["items"]}
    assert after["usb"] is True
    assert after["pixel9"] is True

    # a pause reads idle at once for the mic — its 75s marker window must not
    # keep the dot green after recording stopped
    paused["v"] = True
    stopped = {s["id"]: s["active"] for s in api.sources()["items"]}
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
    empty = {s["id"]: s["active"] for s in api.sources()["items"]}
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
    live = {s["id"]: s["active"] for s in api.sources()["items"]}
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
    paused = {s["id"]: s["active"] for s in api.sources()["items"]}
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
    settled = api.capture_status()
    assert settled["running"] is True
    assert settled["desiredRunning"] is True
    assert settled["settled"] is True
    assert settled["micReachable"] is True

    # Press pause: desired flips NOW; the Mac hasn't applied yet, so the state is
    # transitioning — confirmed still says running, and nothing here contradicts a
    # later poll (this exact shape is what the next poll returns too).
    pausing = api.capture_pause(_capture_request())
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
    confirmed = api.capture_status()
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
    unreachable = api.capture_status()
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
    state = api.capture_pause(_capture_request())
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
    api.capture_pause(_capture_request(client_host="192.168.1.42"))

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


def test_name_voice_endpoint_labels_a_whole_session_voice(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    store = Store.open(tmp_path / "recall.sqlite")
    store.add_source(
        AudioSource(id="meeting-x", name="Meeting", kind=SourceKind.UPLOAD, spec="")
    )
    audio_id = store.add_audio_segment(
        Segment(
            source_id="meeting-x",
            sequence=0,
            start=BASE,
            end=BASE + timedelta(seconds=10),
            path="x",
            sample_rate=48000,
            channels=1,
        )
    )
    ids = [
        store.add_transcript_segment(
            audio_segment_id=audio_id,
            start=BASE + timedelta(seconds=i),
            end=BASE + timedelta(seconds=i + 1),
            text=f"t{i}",
            asr_model="diarized",
            speaker_cluster=c,
        )
        for i, c in enumerate(["SPEAKER_00", "SPEAKER_01"])
    ]
    store.close()

    api.name_voice("meeting-x", VoiceNameIn(cluster="SPEAKER_01", name="Dr Lee"))

    store = Store.open(tmp_path / "recall.sqlite")
    try:
        named = store.get_transcript(ids[1])
        other = store.get_transcript(ids[0])
        assert named is not None and named.speaker_label == "Dr Lee"
        assert other is not None and other.speaker_label is None  # untouched
    finally:
        store.close()


def test_audio_span_returns_one_clip_for_a_run(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A joined bubble plays one continuous clip across all its turns, not just the
    first — the full span from the first turn's start to the last turn's end."""
    flac = tmp_path / "m-20260613T120000.flac"
    make_flac(flac, 10.0)
    store = Store.open(tmp_path / "recall.sqlite")
    store.add_source(AudioSource(id="m", name="m", kind=SourceKind.UPLOAD, spec=""))
    audio_id = store.add_audio_segment(
        Segment(
            source_id="m",
            sequence=0,
            start=BASE,
            end=BASE + timedelta(seconds=10),
            path=str(flac),
            sample_rate=48000,
            channels=1,
        )
    )
    a = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=1),
        end=BASE + timedelta(seconds=2),
        text="one",
        asr_model="diarized",
    )
    b = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=2),
        end=BASE + timedelta(seconds=4),
        text="two",
        asr_model="diarized",
    )
    store.close()
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)

    resp = api.audio_span(from_id=int(a), to_id=int(b))
    assert resp.media_type == "audio/wav"
    assert len(resp.body) > 1000  # real audio for the whole 1s to 4s span


def test_audio_span_rejects_a_span_across_recordings(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    flac1 = tmp_path / "m-20260613T120000.flac"
    flac2 = tmp_path / "m-20260613T120010.flac"
    make_flac(flac1, 5.0)
    make_flac(flac2, 5.0)
    store = Store.open(tmp_path / "recall.sqlite")
    store.add_source(AudioSource(id="m", name="m", kind=SourceKind.UPLOAD, spec=""))
    aid1 = store.add_audio_segment(
        Segment(
            source_id="m",
            sequence=0,
            start=BASE,
            end=BASE + timedelta(seconds=5),
            path=str(flac1),
            sample_rate=48000,
            channels=1,
        )
    )
    aid2 = store.add_audio_segment(
        Segment(
            source_id="m",
            sequence=1,
            start=BASE + timedelta(seconds=10),
            end=BASE + timedelta(seconds=15),
            path=str(flac2),
            sample_rate=48000,
            channels=1,
        )
    )
    a = store.add_transcript_segment(
        audio_segment_id=aid1,
        start=BASE,
        end=BASE + timedelta(seconds=1),
        text="one",
        asr_model="diarized",
    )
    b = store.add_transcript_segment(
        audio_segment_id=aid2,
        start=BASE + timedelta(seconds=10),
        end=BASE + timedelta(seconds=11),
        text="two",
        asr_model="diarized",
    )
    store.close()
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)

    with pytest.raises(HTTPException) as exc:
        api.audio_span(from_id=int(a), to_id=int(b))
    assert exc.value.status_code == 400


def test_session_transcript_endpoint_exports_clean_coalesced(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    store = Store.open(tmp_path / "recall.sqlite")
    store.add_source(AudioSource(id="m", name="M", kind=SourceKind.UPLOAD, spec=""))
    audio = store.add_audio_segment(
        Segment(
            source_id="m",
            sequence=0,
            start=BASE,
            end=BASE + timedelta(minutes=5),
            path="x",
            sample_rate=48000,
            channels=1,
        )
    )
    for i, (text, label) in enumerate(
        [("Hi.", "Pippijn"), ("Yes.", "Pippijn"), ("OK.", "Dr. Adams")]
    ):
        store.add_transcript_segment(
            audio_segment_id=audio,
            start=BASE + timedelta(seconds=i * 10),
            end=BASE + timedelta(seconds=i * 10 + 5),
            text=text,
            asr_model="diarized",
            speaker_label=label,
            speaker_cluster="C",
        )
    store.close()
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)

    out = api.session_transcript("m")
    assert out["session"] == "m"
    assert out["speakers"] == ["Pippijn", "Dr. Adams"]
    # consecutive same-speaker turns are one bubble; current/corrected state only
    assert [t["speaker"] for t in out["turns"]] == ["Pippijn", "Dr. Adams"]
    assert out["turns"][0]["text"] == "Hi. Yes."
    assert out["date"] == out["turns"][0]["start"]


def test_turn_nudge_route_reads_a_json_body_and_moves_the_edge(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Regression: POST /api/turn/{id}/nudge must read {edge,delta} as a JSON *body*. A
    forward-ref ordering bug (NudgeIn defined after the route, under
    `from __future__ import annotations`) once made FastAPI treat `body` as a query
    param → 422. Exercised over HTTP because that resolution only happens there."""
    flac = tmp_path / "usb-20260613T120000.flac"
    make_flac(flac, 30.0)
    store = Store.open(tmp_path / "recall.sqlite")
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
    turn_id = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=5),
        end=BASE + timedelta(seconds=7),
        text="Ja.",
        asr_model="whisper",
        language="nl",
        asr_confidence=0.5,
    )
    store.close()
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)

    client = TestClient(api.app)
    r = client.post(f"/api/turn/{turn_id}/nudge", json={"edge": "start", "delta": -1.0})
    assert r.status_code == 200  # body read as JSON, not demanded as a query param

    store = Store.open(tmp_path / "recall.sqlite")
    moved = next(t for t in store.session_turns("usb") if t.id == turn_id)
    store.close()
    assert moved.start == BASE + timedelta(seconds=4)  # start pulled 1s earlier


def test_malformed_time_is_a_400_not_a_500(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    # A bad ISO time is a client mistake: every endpoint that parses one must
    # answer 400, never let the ValueError escape as a 500. (Guarded by
    # dev-lint's DL-FASTAPI-UNGUARDED-PARSE.)
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    client = TestClient(api.app)

    assert client.get("/api/timeline?before=not-a-time").status_code == 400
    assert client.get("/api/conversations?after=not-a-time").status_code == 400
    assert client.get("/api/train?since=not-a-time").status_code == 400
    refine = client.post(
        "/api/refine", json={"source": "usb", "start": "not-a-time", "end": "x"}
    )
    assert refine.status_code == 400
    ab = client.post("/api/ab-compare", json={"source": "usb", "from": "not-a-time"})
    assert ab.status_code == 400


# --- meeting upload + management (Sessions page) --------------------------------


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


def test_upload_corrects_a_kind_the_worker_already_guessed(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """An upload is authoritative about what it is. If the worker got to the directory
    first — it scans the data root and registers whatever holds segment files — the
    upload must still land as an UPLOAD, or the session is invisible in the list and
    unrenameable/undeletable for ever (`_require_upload` refuses a non-upload)."""
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    store = Store.open(tmp_path / "recall.sqlite")
    store.add_source(  # the worker's guess, placeholder name and all
        AudioSource(
            id="meeting-20260703-1420",
            name="meeting-20260703-1420",
            kind=SourceKind.DISCOVERED,
            spec="",
        )
    )
    store.close()
    client = TestClient(api.app)

    created = _upload_meeting(
        client, tmp_path, title="Dr Lee RT", start="2026-07-03T14:20:00+01:00"
    )

    assert created["id"] == "meeting-20260703-1420"  # same id the worker had claimed
    listed = client.get("/api/sessions").json()["items"]
    assert [i["id"] for i in listed] == ["meeting-20260703-1420"]
    assert listed[0]["title"] == "Dr Lee RT"  # placeholder name replaced, too


def test_create_session_stores_the_mp3_and_lists_it_immediately(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A browser upload of a conversation mp3 becomes a discrete session: the file is
    stored under its own dir (real .mp3, not renamed to .wav), and it shows in the
    sessions list at once (turnCount 0) so it's visibly queued before ASR runs."""
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    client = TestClient(api.app)

    created = _upload_meeting(
        client, tmp_path, title="Dr Lee RT", start="2026-07-03T14:20:00+01:00"
    )
    sid = created["id"]
    assert (
        sid == "meeting-20260703-1420"
    )  # local-time label, as the CLI ingest names them

    stored = list((tmp_path / str(sid)).glob("*.mp3"))
    assert len(stored) == 1  # container preserved, streamed to its own dir

    listed = client.get("/api/sessions").json()["items"]
    row = next(i for i in listed if i["id"] == sid)
    assert row["title"] == "Dr Lee RT"
    assert (
        row["turnCount"] == 0
    )  # present immediately, before the worker transcribes it


def test_create_session_defaults_title_and_rejects_unknown_container(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    client = TestClient(api.app)

    # No title → a sensible default from the local time.
    created = _upload_meeting(client, tmp_path, start="2026-07-03T14:20:00+01:00")
    listed = client.get("/api/sessions").json()["items"]
    row = next(i for i in listed if i["id"] == created["id"])
    assert row["title"] == "Meeting 2026-07-03 14:20"

    # A non-audio upload is refused, not stored as a broken session.
    (tmp_path / "note.txt").write_text("not audio")
    with (tmp_path / "note.txt").open("rb") as fh:
        bad = client.post(
            "/api/sessions", files={"audio": ("note.txt", fh, "text/plain")}
        )
    assert bad.status_code == 400


def test_rename_session_changes_the_displayed_title(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    client = TestClient(api.app)
    sid = _upload_meeting(client, tmp_path)["id"]

    r = client.patch(f"/api/sessions/{sid}", json={"title": "Neuro-oncology clinic"})
    assert r.status_code == 200

    listed = client.get("/api/sessions").json()["items"]
    assert next(i for i in listed if i["id"] == sid)["title"] == "Neuro-oncology clinic"


def test_delete_session_removes_it_and_unlinks_the_audio(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    client = TestClient(api.app)
    sid = _upload_meeting(client, tmp_path)["id"]
    session_dir = tmp_path / str(sid)
    assert session_dir.exists()

    r = client.delete(f"/api/sessions/{sid}")
    assert r.status_code == 200

    listed = client.get("/api/sessions").json()["items"]
    assert all(i["id"] != sid for i in listed)
    assert not session_dir.exists()  # files gone, not just DB rows


def test_delete_refuses_to_touch_household_capture(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The delete path is for uploaded meetings only — it must never be able to erase
    the continuous household archive (which is append-only, never deleted)."""
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    store = Store.open(tmp_path / "recall.sqlite")
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    store.close()
    client = TestClient(api.app)

    r = client.delete("/api/sessions/usb")
    assert r.status_code == 400
    store = Store.open(tmp_path / "recall.sqlite")
    assert store.source_kind("usb") == SourceKind.COREAUDIO  # untouched
    store.close()


def test_rediarize_queues_an_idle_refine_over_the_whole_session(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Re-diarize doesn't run pyannote inline (that would starve capture); it queues a
    refine request the idle daemon drains, spanning the session's full extent."""
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    client = TestClient(api.app)
    sid = _upload_meeting(client, tmp_path, start="2026-07-03T14:20:00+01:00")["id"]

    r = client.post(f"/api/sessions/{sid}/rediarize")
    assert r.status_code == 200

    store = Store.open(tmp_path / "recall.sqlite")
    pending = store.pending_refine_requests()
    store.close()
    assert len(pending) == 1
    assert pending[0].source == sid


def test_refine_route_enqueues_a_request(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """POST /api/refine queues an on-demand refine the idle daemon will pick up."""
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    store = Store.open(tmp_path / "recall.sqlite")
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    store.close()

    client = TestClient(api.app)
    r = client.post(
        "/api/refine",
        json={
            "source": "usb",
            "start": BASE.isoformat(),
            "end": (BASE + timedelta(minutes=5)).isoformat(),
        },
    )
    assert r.status_code == 200

    store = Store.open(tmp_path / "recall.sqlite")
    pending = store.pending_refine_requests()
    store.close()
    assert [(p.source, p.start) for p in pending] == [("usb", BASE)]


def _usb_store(tmp_path: Path) -> Store:
    store = Store.open(tmp_path / "recall.sqlite")
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    return store


def test_ab_compare_enqueue_defaults_to_deployed_pairing(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _usb_store(tmp_path).close()
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    client = TestClient(api.app)

    # No models given → defaults to live stock model vs the adapter-current symlink.
    r = client.post("/api/ab-compare", json={"source": "usb"})
    assert r.status_code == 200
    run_id = r.json()["newId"]

    runs = client.get("/api/ab-compare").json()["items"]
    assert [run["id"] for run in runs] == [run_id]
    run = runs[0]
    assert run["status"] == "queued"
    assert run["source"] == "usb"
    assert run["modelB"].endswith("adapter-current")
    assert run["meanWerA"] is None  # not run yet


def test_ab_compare_enqueue_with_window_and_models(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _usb_store(tmp_path).close()
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    client = TestClient(api.app)

    r = client.post(
        "/api/ab-compare",
        json={
            "source": "usb",
            "from": BASE.isoformat(),
            "to": (BASE + timedelta(minutes=5)).isoformat(),
            "modelA": "large-v3",
            "modelB": "/x/adapter",
            "baseModel": "openai/whisper-large-v3",
        },
    )
    assert r.status_code == 200
    run_id = r.json()["newId"]

    store = Store.open(tmp_path / "recall.sqlite")
    job = store.get_ab_compare_run(run_id)
    store.close()
    assert job is not None
    assert (job.model_a, job.model_b) == ("large-v3", "/x/adapter")
    assert (job.start, job.end) == (BASE, BASE + timedelta(minutes=5))


def test_ab_compare_detail_parses_scores_with_audio_urls(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    store = _usb_store(tmp_path)
    run_id = store.add_ab_compare_run(
        "usb", None, None, model_a="a", model_b="b", base_model="base"
    )
    report = Report(
        model_a="a",
        model_b="b",
        segment_diffs=[SegmentDiff(audio_id=3, start=BASE, text_a="a b", text_b="a c")],
        correction_scores=[
            CorrectionScore(
                correction_id=42,
                truth="a b",
                text_a="a b",
                text_b="a c",
                wer_a=0.0,
                wer_b=0.5,
            )
        ],
    )
    store.save_ab_compare_result(
        run_id,
        result_json=render_json(report),
        mean_wer_a=report.mean_wer_a,
        mean_wer_b=report.mean_wer_b,
        n_corrections=1,
        n_segments=1,
        n_changed=1,
    )
    store.close()
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    client = TestClient(api.app)

    detail = client.get(f"/api/ab-compare/{run_id}").json()
    assert detail["summary"]["status"] == "done"
    assert detail["summary"]["meanWerB"] == 0.5
    assert len(detail["scores"]) == 1
    score = detail["scores"][0]
    assert score["correctionId"] == 42
    assert score["werA"] == 0.0
    assert score["audioUrl"] == "/api/correction/42/audio"
    assert detail["segmentDiffs"][0] == {
        "audioId": 3,
        "start": BASE.isoformat(),
        "changed": True,
        "textA": "a b",
        "textB": "a c",
    }


def test_ab_compare_detail_404_for_unknown_run(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _usb_store(tmp_path).close()
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    client = TestClient(api.app)
    assert client.get("/api/ab-compare/9999").status_code == 404


def test_split_rejects_unparseable_fragment_times(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # Substituting now() for a missing fragment time would plant today's timestamp
    # inside an old recording — export_corpus would then slice the wrong audio into
    # the fine-tune corpus. Malformed times must be a 400, not manufactured data.
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    store = Store.open(tmp_path / "recall.sqlite")
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    audio_id = store.add_audio_segment(
        Segment(
            source_id="usb",
            sequence=0,
            start=BASE,
            end=BASE + timedelta(seconds=60),
            path="x.flac",
            sample_rate=48000,
            channels=1,
        )
    )
    turn_id = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=4),
        text="hello there world",
        asr_model="whisper",
    )
    store.close()

    client = TestClient(api.app)
    r = client.post(
        "/api/split",
        json={
            "id": turn_id,
            "fragments": [
                {
                    "start": "not-a-date",
                    "end": "also-not",
                    "text": "hello",
                    "speaker": "A",
                },
            ],
        },
    )
    assert r.status_code == 400

    store = Store.open(tmp_path / "recall.sqlite")
    unchanged = store.get_transcript(turn_id)
    store.close()
    assert unchanged is not None
    assert unchanged.superseded_by is None  # nothing was written


def _seed_ask_store(tmp_path: Path) -> None:
    store = Store.open(tmp_path / "recall.sqlite")
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    audio_id = store.add_audio_segment(
        Segment(
            source_id="usb",
            sequence=0,
            start=BASE,
            end=BASE + timedelta(seconds=60),
            path="x",
            sample_rate=48000,
            channels=1,
        )
    )
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=2),
        text="the plumber is coming on Thursday",
        asr_model="whisper",
        speaker_label="Alice",
    )
    store.set_day_summary("2026-06-13", "Plans were made.", model="test-llm")
    store.close()


def test_summaries_endpoint_lists_recent_days(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    _seed_ask_store(tmp_path)
    client = TestClient(api.app)
    r = client.get("/api/summaries")
    assert r.status_code == 200
    assert r.json()["items"] == [
        {"day": "2026-06-13", "text": "Plans were made.", "model": "test-llm"}
    ]


def test_ask_answers_with_cited_turns(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    _seed_ask_store(tmp_path)
    # Stub the process-wide generator: no model load in tests.
    monkeypatch.setattr(api, "_llm", lambda _p: "Thursday, per Alice.")
    client = TestClient(api.app)
    r = client.post("/api/ask", json={"question": "When is the plumber coming?"})
    assert r.status_code == 200
    body = r.json()
    assert body["answer"] == "Thursday, per Alice."
    assert [t["text"] for t in body["sources"]] == ["the plumber is coming on Thursday"]


def test_ask_declines_without_evidence_and_never_generates(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    _seed_ask_store(tmp_path)

    def explode(_p: str) -> str:
        msg = "generator must not run without evidence"
        raise AssertionError(msg)

    monkeypatch.setattr(api, "_llm", explode)
    client = TestClient(api.app)
    r = client.post("/api/ask", json={"question": "anything about zeppelins?"})
    assert r.status_code == 200
    assert r.json() == {
        "status": "done",
        "id": None,
        "answer": None,
        "sources": [],
        "error": None,
    }
    assert client.post("/api/ask", json={"question": "   "}).status_code == 400


def test_ask_on_fleet_queues_a_job_and_the_poll_resolves_it(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # On the fleet (Isis has no MLX) POST /api/ask must NOT generate: it retrieves,
    # queues the built prompt for the Mac, and returns a poll id with the cited sources
    # shown immediately. GET /api/ask/{id} is pending until the Mac lands the answer.
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    monkeypatch.setenv("RECALL_ROLE", "fleet")
    _seed_ask_store(tmp_path)

    def explode(_p: str) -> str:
        raise AssertionError("the fleet must never run the generator")

    monkeypatch.setattr(api, "_llm", explode)
    client = TestClient(api.app)

    r = client.post("/api/ask", json={"question": "When is the plumber coming?"})
    assert r.status_code == 200
    body = r.json()
    assert body["status"] == "pending"
    rid = body["id"]
    assert isinstance(rid, int)
    # sources are shown while it waits
    assert [t["text"] for t in body["sources"]] == ["the plumber is coming on Thursday"]

    poll = client.get(f"/api/ask/{rid}").json()
    assert poll["status"] == "pending" and poll["answer"] is None

    # The Mac lands the answer (the relay's push-back); the poll now resolves.
    store = Store.open(tmp_path / "recall.sqlite")
    store.save_ask_answer(rid, "Thursday, per Alice.")
    store.close()
    done = client.get(f"/api/ask/{rid}").json()
    assert done["status"] == "done"
    assert done["answer"] == "Thursday, per Alice."
    assert [t["text"] for t in done["sources"]] == ["the plumber is coming on Thursday"]

    assert client.get("/api/ask/999999").status_code == 404


def test_ask_poll_surfaces_a_generation_error(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    monkeypatch.setenv("RECALL_ROLE", "fleet")
    _seed_ask_store(tmp_path)
    monkeypatch.setattr(api, "_llm", lambda _p: "unused")
    client = TestClient(api.app)
    rid = client.post(
        "/api/ask", json={"question": "When is the plumber coming?"}
    ).json()["id"]
    store = Store.open(tmp_path / "recall.sqlite")
    store.mark_ask_error(rid, "model failed to load")
    store.close()
    poll = client.get(f"/api/ask/{rid}").json()
    assert poll["status"] == "error" and poll["error"] == "model failed to load"


def test_ask_poll_times_out_a_job_the_mac_never_answers(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # A pending job older than the backstop must report a timeout instead of spinning
    # "Thinking…" forever (Mac offline/wedged). The row is left intact.
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    monkeypatch.setenv("RECALL_ROLE", "fleet")
    monkeypatch.setattr(api, "_ASK_TIMEOUT_SECONDS", 0)  # anything pending is "too old"
    _seed_ask_store(tmp_path)
    monkeypatch.setattr(api, "_llm", lambda _p: "unused")
    client = TestClient(api.app)
    rid = client.post(
        "/api/ask", json={"question": "When is the plumber coming?"}
    ).json()["id"]
    poll = client.get(f"/api/ask/{rid}").json()
    assert poll["status"] == "error"
    assert "Timed out" in poll["error"]
    # the row is untouched — a late answer could still land for a fresh ask
    store = Store.open(tmp_path / "recall.sqlite")
    assert store.get_ask_request(rid) is not None
    assert not store.get_ask_request(rid).done  # type: ignore[union-attr]
    store.close()


def test_vocabulary_round_trip(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    Store.open(tmp_path / "recall.sqlite").close()
    client = TestClient(api.app)

    new_id = client.post("/api/vocabulary", json={"term": " EGA wing "}).json()["newId"]
    client.post("/api/vocabulary", json={"term": "vorasidenib"})
    listed = client.get("/api/vocabulary").json()["items"]
    assert [t["term"] for t in listed] == ["EGA wing", "vorasidenib"]

    assert client.post("/api/vocabulary", json={"term": "  "}).status_code == 400
    assert client.delete(f"/api/vocabulary/{new_id}").json() == {"ok": True}
    assert [t["term"] for t in client.get("/api/vocabulary").json()["items"]] == [
        "vorasidenib"
    ]


# --- "today so far" live summary ------------------------------------------------


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


def _sync_refresh(monkeypatch: pytest.MonkeyPatch) -> None:
    """Run the background refresh inline, so tests are deterministic."""
    monkeypatch.setattr(api, "_start_today_refresh", api._refresh_today_worker)


def test_today_summary_is_empty_when_nothing_was_recorded(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    Store.open(tmp_path / "recall.sqlite").close()  # empty DB

    def explode(_p: str) -> str:
        msg = "must not generate for an empty day"
        raise AssertionError(msg)

    monkeypatch.setattr(api, "_llm", explode)
    _sync_refresh(monkeypatch)
    client = TestClient(api.app)

    body = client.get("/api/summaries/today").json()
    assert body["text"] is None
    assert body["upToDate"] is True
    assert body["pending"] is False


def test_today_summary_serves_stale_and_revalidates(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Stale-while-revalidate: the first request returns immediately (no text yet,
    refresh kicked off); once the refresh lands the next request serves it fresh —
    and it is NOT regenerated again while nothing new was said."""
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    _seed_today(tmp_path, ["the plumber is coming on Thursday at nine"])
    calls = 0

    def generator(_p: str) -> str:
        nonlocal calls
        calls += 1
        return "So far: plumber Thursday nine."

    monkeypatch.setattr(api, "_llm", generator)
    _sync_refresh(monkeypatch)
    client = TestClient(api.app)

    first = client.get("/api/summaries/today").json()
    assert first["text"] is None  # nothing cached yet — served without waiting
    assert first["pending"] is True

    second = client.get("/api/summaries/today").json()
    assert second["text"] == "So far: plumber Thursday nine."
    assert second["upToDate"] is True
    assert second["pending"] is False
    assert second["generatedAt"]  # the UI shows "as of HH:MM"

    client.get("/api/summaries/today")
    assert calls == 1  # cache hit — no regeneration while nothing new was said


def test_today_summary_goes_stale_when_a_new_turn_lands(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    _seed_today(tmp_path, ["morning words"])
    outputs = iter(["Morning only.", "Morning and afternoon."])
    monkeypatch.setattr(api, "_llm", lambda _p: next(outputs))
    _sync_refresh(monkeypatch)
    client = TestClient(api.app)

    client.get("/api/summaries/today")  # generates "Morning only."
    fresh = client.get("/api/summaries/today").json()
    assert (fresh["text"], fresh["upToDate"]) == ("Morning only.", True)

    _seed_today(tmp_path, ["afternoon words"])  # a new turn moves the watermark

    stale = client.get("/api/summaries/today").json()
    assert stale["text"] == "Morning only."  # old text served immediately...
    assert stale["upToDate"] is False  # ...but marked stale
    assert stale["pending"] is True  # ...and a refresh was kicked off

    updated = client.get("/api/summaries/today").json()
    assert (updated["text"], updated["upToDate"]) == ("Morning and afternoon.", True)


def test_context_roundtrip_over_http(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The household context (background facts for the LLM) is data, edited from
    the web — GET returns what PUT stored; empty until set."""
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    Store.open(tmp_path / "recall.sqlite").close()
    client = TestClient(api.app)

    assert client.get("/api/context").json() == {"text": ""}
    r = client.put("/api/context", json={"text": "Alice is left-handed."})
    assert r.status_code == 200
    assert client.get("/api/context").json() == {"text": "Alice is left-handed."}


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


def test_a_second_scan_request_joins_the_running_one(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Two tabs (or two clicks) must not run two scans: they'd decode the same files
    twice and race each other's writes. The second request joins the first."""
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    monkeypatch.setattr("recall.api_quiet._SCAN_JOB", None)
    _deaf(monkeypatch)
    store = Store.open(tmp_path / "recall.sqlite")
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    base = datetime(2026, 7, 11, 9, 0, 0, tzinfo=UTC)
    for i in range(4):
        start = base + timedelta(seconds=i * 59)
        (tmp_path / f"seg{i}.opus").write_bytes(b"audio")
        audio_id = store.add_audio_segment(
            Segment(
                source_id="usb",
                sequence=i,
                start=start,
                end=start + timedelta(seconds=59),
                path=str(tmp_path / f"seg{i}.opus"),
                sample_rate=48000,
                channels=1,
            )
        )
        store.mark_transcribed(int(audio_id))
    store.close()

    decoded: list[str] = []

    def measure_once(path: Path) -> Measurement:
        decoded.append(str(path))
        time.sleep(0.05)  # long enough that the second request lands mid-scan
        return Measurement(mean_db=-62.0, buckets=(-62.0,) * 600)

    monkeypatch.setattr("recall.quiet.measure", measure_once)

    client = TestClient(api.app)
    client.post("/api/quiet/scan")
    client.post("/api/quiet/scan")  # a second tab, while the first is still going
    _await_scan(client)

    assert sorted(decoded) == sorted(str(tmp_path / f"seg{i}.opus") for i in range(4))


def test_quiet_scan_spans_and_delete(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # End-to-end cleanup: scan measures raw volume, spans surfaces the long quiet run,
    # delete removes its rows AND unlinks the Opus files (the true deletion).
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    store = Store.open(tmp_path / "recall.sqlite")
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    base = datetime(2026, 7, 11, 9, 0, 0, tzinfo=UTC)
    for i in range(8):
        start = base + timedelta(seconds=i * 59)
        (tmp_path / f"seg{i}.opus").write_bytes(b"audio")
        audio_id = store.add_audio_segment(
            Segment(
                source_id="usb",
                sequence=i,
                start=start,
                end=start + timedelta(seconds=59),
                path=str(tmp_path / f"seg{i}.opus"),
                sample_rate=48000,
                channels=1,
            )
        )
        store.mark_transcribed(int(audio_id))  # only what ASR has read can be swept
    store.close()
    vols = {
        str(tmp_path / f"seg{i}.opus"): (-62.0 if i < 6 else -50.0) for i in range(8)
    }
    monkeypatch.setattr(
        "recall.quiet.measure",
        lambda p: Measurement(mean_db=vols[str(p)], buckets=(vols[str(p)],) * 600),
    )
    monkeypatch.setattr(
        "recall.api_quiet._SCAN_JOB", None
    )  # a fresh job, bound to this data root
    # Speech in the last two segments — and *only* the detector says so. Their volume
    # says nothing the detector doesn't: a minute of far-field speech and a minute with
    # a door closing in it look the same on a 60-second mean, which is why the mean is
    # not allowed to decide (recall.quiet).
    monkeypatch.setattr(
        "recall.analyse.silero_speech_regions",
        lambda p: (
            [SpeechRegion(start=3.0, end=9.0)]
            if p.name in {"seg6.opus", "seg7.opus"}
            else []
        ),
    )

    client = TestClient(api.app)
    scan = _await_scan(client)
    assert (scan["measured"], scan["total"], scan["running"]) == (8, 8, False)
    assert scan["analysed"] == scan["toAnalyse"]  # every candidate was listened to
    items = client.get("/api/quiet/spans", params={"min_seconds": 300}).json()["items"]
    assert len(items) == 1
    assert len(items[0]["audioIds"]) == 6  # the 6 the detector cleared (354s > 300s)
    assert items[0]["source"] == "usb"

    # The waveform behind the review is READ, not decoded: the scan already decoded
    # every file, and re-decoding a 100-minute span's 130 files each time it is opened
    # is what made this page unusable. Any call to ffmpeg here is a bug.
    def _must_not_decode(path: str) -> tuple[float, ...]:
        raise AssertionError(
            f"the review decoded {path}; it must read the stored shape"
        )

    monkeypatch.setattr("recall.envelope.segment_envelope", _must_not_decode)

    envelope = client.get(
        "/api/quiet/envelope",
        params={
            "source": "usb",
            "start": base.isoformat(),
            "end": (base + timedelta(seconds=8 * 59)).isoformat(),
        },
    ).json()
    # A *sound* is judged at the level measured for THIS mic (recall.calibrate), not at
    # the detector's -60 dB mean — the noise floor's 0.1s crests cross -60 constantly.
    # This source is too new to have been measured, so it falls back to the default. The
    # line the UI draws is the one the list uses, so picture and list agree.
    assert envelope["thresholdDb"] == DEFAULT_EVENT_DB
    assert len(envelope["segments"]) == 8
    # The events are the loud segments (6 and 7), joined into one run of sound — the
    # reviewer is shown what broke the quiet, not left to find it.
    assert len(envelope["events"]) == 1
    assert envelope["events"][0]["peakDb"] == -50.0
    assert envelope["events"][0]["start"].startswith("2026-07-11T09:05:54")

    deleted = client.post(
        "/api/quiet/delete", json={"audioIds": items[0]["audioIds"]}
    ).json()
    assert deleted["deleted"] == 6
    assert deleted["freedBytes"] == 6 * len(b"audio")
    for i in range(6):
        assert not (tmp_path / f"seg{i}.opus").exists()  # files really gone
    assert client.get("/api/quiet/spans").json()["items"] == []  # nothing left to prune


def test_the_review_list_leads_with_the_biggest_span(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The endpoint must actually *use* the ranking, not merely have one available.

    `rank_spans` is unit-tested, but the sort it replaced lived inline in this
    endpoint — so the API could quietly stop calling it and every other test would still
    pass, while the page went back to leading with a six-minute shard over a silent
    hour. This is the only test that fails if that wiring is cut.
    """
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    monkeypatch.setattr("recall.api_quiet._SCAN_JOB", None)
    _deaf(monkeypatch)
    store = Store.open(tmp_path / "recall.sqlite")
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    base = datetime(2026, 7, 11, 9, 0, 0, tzinfo=UTC)

    # A short, spotless span, then a gap, then a long one holding a single bump. The
    # short one wins on every measure of purity and must still come second.
    def add(i: int, at: float) -> None:
        start = base + timedelta(seconds=at)
        (tmp_path / f"seg{i}.opus").write_bytes(b"audio")
        audio_id = store.add_audio_segment(
            Segment(
                source_id="usb",
                sequence=i,
                start=start,
                end=start + timedelta(seconds=59),
                path=str(tmp_path / f"seg{i}.opus"),
                sample_rate=48000,
                channels=1,
            )
        )
        store.mark_transcribed(int(audio_id))

    for i in range(6):  # the shard: 6 minutes
        add(i, i * 59)
    for i in range(6, 20):  # the prize: 14 minutes, after an hour-long hole
        add(i, 3600 + (i - 6) * 59)
    store.close()

    def measured(path: object) -> Measurement:
        bump = str(path).endswith("seg10.opus")  # one door closing, in the long span
        buckets = (-66.0,) * 600
        if bump:
            # 0.6s of door in a 60s minute. Brief on purpose: a bump long enough to sit
            # above the mic's *own* 99.9th percentile would redefine that mic's floor as
            # "doors", and then no door is ever a sound again.
            buckets = (-40.0,) * 6 + (-66.0,) * 594
        return Measurement(mean_db=-66.0, buckets=buckets)

    monkeypatch.setattr("recall.quiet.measure", measured)

    client = TestClient(api.app)
    _await_scan(client)
    items = client.get("/api/quiet/spans", params={"min_seconds": 300}).json()["items"]

    assert len(items) == 2
    assert items[0]["durationS"] > items[1]["durationS"]  # biggest first
    assert len(items[0]["audioIds"]) == 14  # the long one, bump and all
    assert not items[0]["silent"]  # it is honest about the bump...
    assert items[1]["silent"]  # ...and the spotless shard still comes second


def test_an_undecodable_segment_is_examined_once_and_drawn_as_a_gap(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The archive holds a truncated file (capture died mid-write). End to end: the scan
    records the verdict, so the archive reads as fully measured and it is never decoded
    again; the review draws it as a gap; and it is never offered for deletion."""
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    monkeypatch.setattr("recall.api_quiet._SCAN_JOB", None)
    _deaf(monkeypatch)
    store = Store.open(tmp_path / "recall.sqlite")
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    base = datetime(2026, 7, 11, 9, 0, 0, tzinfo=UTC)
    for i in range(3):
        start = base + timedelta(seconds=i * 60)
        (tmp_path / f"seg{i}.opus").write_bytes(b"audio")
        audio_id = store.add_audio_segment(
            Segment(
                source_id="usb",
                sequence=i,
                start=start,
                end=start + timedelta(seconds=60),
                path=str(tmp_path / f"seg{i}.opus"),
                sample_rate=48000,
                channels=1,
            )
        )
        store.mark_transcribed(int(audio_id))
    store.close()

    broken = str(tmp_path / "seg1.opus")  # the middle minute will not decode
    decoded: list[str] = []

    def measure_one(path: Path) -> Measurement | None:
        decoded.append(str(path))
        if str(path) == broken:
            return None
        return Measurement(mean_db=-62.0, buckets=(-62.0,) * 600)

    monkeypatch.setattr("recall.quiet.measure", measure_one)

    client = TestClient(api.app)
    # Every segment is examined, including the broken one — the archive reads as done.
    scan = _await_scan(client)
    assert (scan["measured"], scan["total"], scan["running"]) == (3, 3, False)

    _await_scan(client)  # a second scan finds nothing left to do...
    assert sorted(decoded) == sorted(str(tmp_path / f"seg{i}.opus") for i in range(3))

    # ...and the review reads the verdict rather than trying the file again.
    def _must_not_decode(path: str) -> tuple[float, ...]:
        raise AssertionError(f"the review decoded {path}")

    monkeypatch.setattr("recall.envelope.segment_envelope", _must_not_decode)
    envelope = client.get(
        "/api/quiet/envelope",
        params={
            "source": "usb",
            "start": base.isoformat(),
            "end": (base + timedelta(seconds=180)).isoformat(),
            "max_points": 1800,
        },
    ).json()
    minutes = [envelope["points"][i * 600 : (i + 1) * 600] for i in range(3)]
    assert all(v == -62.0 for v in minutes[0])
    assert all(v is None for v in minutes[1])  # the broken minute: a gap, not silence
    assert all(v == -62.0 for v in minutes[2])

    # No volume, so it stays unknown — it splits the quiet rather than joining it.
    spans = client.get("/api/quiet/spans", params={"min_seconds": 30}).json()["items"]
    assert len(spans) == 2
    assert all(len(s["audioIds"]) == 1 for s in spans)


def test_quiet_never_offers_a_segment_that_still_bears_a_turn(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # The failure this guards against, seen on the real archive: quiet far-field speech
    # keeps a minute's mean under the noise-floor bar, and deleting the audio would take
    # the transcript with it. The turn wins over the volume, end to end.
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    store = Store.open(tmp_path / "recall.sqlite")
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    base = datetime(2026, 7, 11, 9, 0, 0, tzinfo=UTC)
    audio_ids = []
    for i in range(8):
        start = base + timedelta(seconds=i * 59)
        (tmp_path / f"seg{i}.opus").write_bytes(b"audio")
        audio_id = store.add_audio_segment(
            Segment(
                source_id="usb",
                sequence=i,
                start=start,
                end=start + timedelta(seconds=59),
                path=str(tmp_path / f"seg{i}.opus"),
                sample_rate=48000,
                channels=1,
            )
        )
        store.mark_transcribed(int(audio_id))
        audio_ids.append(audio_id)
    # Segment 3 transcribed a quiet Dutch sentence; the audio is still under the bar.
    store.add_transcript_segment(
        audio_segment_id=int(audio_ids[3]),
        start=base + timedelta(seconds=3 * 59),
        end=base + timedelta(seconds=3 * 59 + 4),
        text="Namelijk, dit zijn ook al vlakjes",
        asr_model="whisper",
    )
    store.close()
    monkeypatch.setattr(
        "recall.quiet.measure",
        lambda _p: Measurement(mean_db=-62.0, buckets=(-62.0,) * 600),
    )
    monkeypatch.setattr("recall.api_quiet._SCAN_JOB", None)
    _deaf(monkeypatch)

    client = TestClient(api.app)
    _await_scan(client)
    items = client.get("/api/quiet/spans", params={"min_seconds": 100}).json()["items"]

    swept = {a for span in items for a in span["audioIds"]}
    assert int(audio_ids[3]) not in swept  # the audio under the words survives
    assert (tmp_path / "seg3.opus").exists()


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


def test_a_phone_can_say_what_it_could_not_upload(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """The gap #77 closes, end to end through the routes.

    An approved recording the phone cannot deliver was state no fleet component
    could see: the meeting recorder 401ed from the day it was written and said
    "N recordings waiting to upload" throughout, which is what it also says when
    you are simply not home yet.
    """
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    Store.open(tmp_path / "recall.sqlite").close()
    client = TestClient(api.app)

    posted = client.post(
        "/api/devices/outbox",
        json={
            "device": "pixel9",
            "queued": 2,
            "oldestQueuedAt": "2026-08-10T09:00:00+00:00",
            "failing": 2,
            "reason": "Not authorised — check the upload token in Settings.",
        },
    )
    assert posted.status_code == 200

    [item] = client.get("/api/devices/outbox").json()["items"]
    assert item["device"] == "pixel9"
    assert item["queued"] == 2
    assert item["oldestQueuedAt"].startswith("2026-08-10T09:00")
    assert "token" in item["reason"], "the fleet gets the phone's own diagnosis"
    assert item["at"], "and when it said so, which is what staleness is judged on"


def test_a_phone_with_nothing_queued_still_reports(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    # The clearing signal. Without it the last bad reading stands forever and the
    # check can never go back to green, which is how a check gets muted.
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    Store.open(tmp_path / "recall.sqlite").close()
    client = TestClient(api.app)

    client.post(
        "/api/devices/outbox", json={"device": "pixel9", "queued": 3, "failing": 3}
    )
    client.post("/api/devices/outbox", json={"device": "pixel9"})

    [item] = client.get("/api/devices/outbox").json()["items"]
    assert (item["queued"], item["failing"], item["oldestQueuedAt"]) == (0, 0, None)


def test_an_unparseable_time_from_a_phone_is_dropped_not_a_500(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    # Best-effort status, never control: an older build must cost its own field,
    # not the endpoint. (The 400 rule above is for the browsing plane, where a bad
    # time means the caller asked a question that cannot be answered.)
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    Store.open(tmp_path / "recall.sqlite").close()
    client = TestClient(api.app)

    posted = client.post(
        "/api/devices/outbox",
        json={"device": "pixel9", "queued": 1, "oldestQueuedAt": "yesterday-ish"},
    )
    assert posted.status_code == 200
    [item] = client.get("/api/devices/outbox").json()["items"]
    assert item["queued"] == 1
    assert item["oldestQueuedAt"] is None


def test_a_mic_app_beat_round_trips_through_the_api(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """The signal that survives a paused household and a silent room (#837).

    recall's own liveness marker cannot answer this: it is refreshed only by audio
    above the silence floor, so a quiet room reads idle, and while capture is paused
    the ingest listener is closed and nothing streams at all.
    """
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    Store.open(tmp_path / "recall.sqlite").close()
    client = TestClient(api.app)

    posted = client.post(
        "/api/devices/heartbeat",
        json={
            "device": "iphone11",
            "app": "ios",
            "version": "1.4.0 (37)",
            "startedAt": "2026-08-11T07:00:00+00:00",
            "streaming": False,
            "charging": True,
        },
    )
    assert posted.status_code == 200

    [item] = client.get("/api/devices/heartbeat").json()["items"]
    assert item["device"] == "iphone11"
    assert item["app"] == "ios"
    assert item["startedAt"].startswith("2026-08-11T07:00")
    assert item["streaming"] is False, "a paused app is still an alive app"
    assert item["charging"] is True
    assert item["at"], "and when the FLEET heard it, which staleness is judged on"


def test_the_beats_clock_is_the_servers_not_the_phones(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """⚠ A phone with a wrong clock must not be able to grade itself.

    `at` is stamped on arrival. Taking the phone's word for it would let a device
    with a skewed clock report itself permanently fresh — or permanently stale,
    raising an alarm about hardware that is fine.
    """
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    Store.open(tmp_path / "recall.sqlite").close()
    client = TestClient(api.app)

    before = datetime.now(UTC)
    client.post(
        "/api/devices/heartbeat",
        json={"device": "pixel5", "startedAt": "1999-01-01T00:00:00+00:00"},
    )
    [item] = client.get("/api/devices/heartbeat").json()["items"]
    assert datetime.fromisoformat(item["at"]) >= before
    assert item["startedAt"].startswith("1999"), "the phone's own time is kept as-is"


def test_a_beat_from_an_older_build_still_counts_as_alive(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    # Everything but the device id is optional on purpose: the beat ARRIVING is the
    # finding, and an app that cannot name its version is not thereby dead.
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    Store.open(tmp_path / "recall.sqlite").close()
    client = TestClient(api.app)

    posted = client.post("/api/devices/heartbeat", json={"device": "pixel5"})
    assert posted.status_code == 200
    [item] = client.get("/api/devices/heartbeat").json()["items"]
    assert (item["device"], item["startedAt"], item["charging"]) == (
        "pixel5",
        None,
        None,
    )


def test_an_unparseable_start_time_costs_that_field_not_the_beat(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.setattr(api, "DATA_ROOT", tmp_path)
    Store.open(tmp_path / "recall.sqlite").close()
    client = TestClient(api.app)

    posted = client.post(
        "/api/devices/heartbeat",
        json={"device": "pixel5", "startedAt": "since-tuesday"},
    )
    assert posted.status_code == 200
    [item] = client.get("/api/devices/heartbeat").json()["items"]
    assert item["startedAt"] is None
