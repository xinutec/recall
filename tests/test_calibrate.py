"""What counts as a sound on a given microphone, measured from that microphone."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta

import pytest

from recall.calibrate import (
    MIN_QUIET_SEGMENTS,
    SPEECH_MARGIN_DB,
    calibrate,
    calibrate_source,
    event_threshold,
)
from recall.envelope import DEFAULT_EVENT_DB, Measurement, encode_envelope
from recall.quiet import scan_segments
from recall.sources import AudioSource, SourceKind
from recall.store import Store
from recall.timeline import Segment

BASE = datetime(2026, 7, 12, 9, 0, 0, tzinfo=UTC)


def _envelope(*levels: float, length: int = 600) -> bytes:
    """A minute-long envelope: `levels` at the front, the rest at a deep floor."""
    buckets = [*levels, *([-95.0] * (length - len(levels)))]
    return encode_envelope(buckets)


def _floor(peak: float) -> bytes:
    """Idle mic: a deep floor whose loudest crest reaches `peak`."""
    return _envelope(peak, peak - 3, peak - 6)


def test_the_floor_sets_the_bar_on_a_mic_that_hears_clearly() -> None:
    # The USB mic: its faint speech (-50) sits above its floor's crests (-52), so the
    # floor decides and the sound list stays short.
    quiet = [_floor(-52.0)] * 50
    speech = [_envelope(-50.0)]
    result = calibrate_source("usb", quiet, speech)

    assert result is not None
    assert result.floor_ceiling_db == pytest.approx(-52.0, abs=0.5)
    assert result.faintest_speech_db == pytest.approx(-50.0, abs=0.1)
    assert result.threshold_db == pytest.approx(-52.0, abs=0.5)
    assert not result.bounded_by_speech


def test_faint_speech_pulls_the_bar_down_on_a_mic_that_hears_badly() -> None:
    """The phones: their words peak at -70 dB, *below* their own floor's crests (-62).

    A threshold taken from the floor alone would sit above every word the mic has ever
    recorded — the review would say "no sound at all in this span" over full sentences.
    Speech wins the tie, always: a long list costs clicks, a short one costs words.
    """
    quiet = [_floor(-62.0)] * 50
    speech = [_envelope(-70.0), _envelope(-58.0)]
    result = calibrate_source("pixel5", quiet, speech)

    assert result is not None
    assert result.floor_ceiling_db == pytest.approx(-62.0, abs=0.5)
    assert result.threshold_db == pytest.approx(-70.0 - SPEECH_MARGIN_DB, abs=0.1)
    assert result.bounded_by_speech
    assert result.threshold_db < result.faintest_speech_db  # type: ignore[operator]


def test_a_mic_that_has_never_heard_speech_is_bounded_by_its_floor_alone() -> None:
    result = calibrate_source("newmic", [_floor(-58.0)] * 50, [])
    assert result is not None
    assert result.faintest_speech_db is None
    assert result.threshold_db == pytest.approx(-58.0, abs=0.5)


def test_a_barely_heard_mic_is_not_calibrated_at_all() -> None:
    # Too few segments and the percentiles are noise. Better to fall back to the default
    # than to commit to a number the data cannot support.
    assert (
        calibrate_source("newmic", [_floor(-60.0)] * (MIN_QUIET_SEGMENTS - 1), [])
        is None
    )


def _store_with(source_id: str, kind: SourceKind, n: int) -> Store:
    store = Store.memory()
    store.add_source(AudioSource(id=source_id, name=source_id, kind=kind, spec=""))
    for i in range(n):
        start = BASE + timedelta(seconds=i * 60)
        audio_id = store.add_audio_segment(
            Segment(
                source_id=source_id,
                sequence=i,
                start=start,
                end=start + timedelta(seconds=60),
                path=f"/archive/{source_id}/seg{i:03d}.opus",
                sample_rate=48000,
                channels=1,
            )
        )
        store.mark_transcribed(int(audio_id))
    return store


def test_calibration_is_measured_and_stored_per_source(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # Two mics recording the same room, 15 dB apart. Each must get its own threshold —
    # one constant across both is what hid real words on the quieter one.
    store = Store.memory()
    for source_id, kind in (
        ("usb", SourceKind.COREAUDIO),
        ("pixel5", SourceKind.TCP_PCM),
    ):
        store.add_source(AudioSource(id=source_id, name=source_id, kind=kind, spec=""))
        for i in range(30):
            start = BASE + timedelta(seconds=i * 60)
            audio_id = store.add_audio_segment(
                Segment(
                    source_id=source_id,
                    sequence=i,
                    start=start,
                    end=start + timedelta(seconds=60),
                    path=f"/archive/{source_id}/seg{i:03d}.opus",
                    sample_rate=48000,
                    channels=1,
                )
            )
            store.mark_transcribed(int(audio_id))

    def measure(path: object) -> Measurement:
        floor = -62.0 if "/usb/" in str(path) else -77.0
        return Measurement(mean_db=floor - 5, buckets=(floor - 8,) * 597 + (floor,) * 3)

    monkeypatch.setattr("recall.quiet.measure", measure)
    scan_segments(store)

    results = {c.source_id: c for c in calibrate(store)}
    assert set(results) == {"usb", "pixel5"}
    assert results["usb"].threshold_db == pytest.approx(-62.0, abs=1.0)
    assert results["pixel5"].threshold_db == pytest.approx(-77.0, abs=1.0)
    # And the review reads each mic's own number back.
    assert event_threshold(store, "usb") == pytest.approx(-62.0, abs=1.0)
    assert event_threshold(store, "pixel5") == pytest.approx(-77.0, abs=1.0)


def test_an_unmeasured_source_falls_back_to_the_default() -> None:
    store = _store_with("brandnew", SourceKind.TCP_PCM, 1)
    assert event_threshold(store, "brandnew") == DEFAULT_EVENT_DB


def test_uploaded_meetings_are_not_calibrated() -> None:
    # A meeting is never swept, so it has no sound list to threshold.
    store = _store_with("meeting-1", SourceKind.UPLOAD, 30)
    assert store.sweepable_source_ids() == []
    assert calibrate(store) == []
