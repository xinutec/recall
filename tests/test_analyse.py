"""The speech detector is the veto: what a segment *contains*, not how loud it was."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest

from recall.analyse import analyse_segments
from recall.envelope import encode_envelope
from recall.ids import AudioSegmentId
from recall.quiet import SWEEPABLE_KINDS, quiet_spans
from recall.sources import AudioSource, SourceKind
from recall.store import Store
from recall.timeline import Segment
from recall.vad import SpeechRegion

BASE = datetime(2026, 7, 12, 9, 0, 0, tzinfo=UTC)


def _store(n: int) -> tuple[Store, list[int]]:
    store = Store.memory()
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    ids = []
    for i in range(n):
        start = BASE + timedelta(seconds=i * 59)
        audio_id = store.add_audio_segment(
            Segment(
                source_id="usb",
                sequence=i,
                start=start,
                end=start + timedelta(seconds=59),
                path=f"/archive/usb/seg{i:03d}.opus",
                sample_rate=48000,
                channels=1,
            )
        )
        store.mark_transcribed(int(audio_id))
        # An idle minute, envelope and all. The envelope is not decoration: it is
        # what says the room was empty (b'\x00\x00' would decode to 0 dB — a minute
        # of full-scale noise, which is not what 'quiet by volume' means).
        store.set_audio_measurement(audio_id, -62.0, encode_envelope((-62.0,) * 590))
        ids.append(int(audio_id))
    return store, ids


def _silent(_path: Path) -> list[SpeechRegion]:
    return []


def test_a_segment_the_detector_hears_speech_in_is_never_swept() -> None:
    """The failure this whole veto exists for.

    On the real archive a reprocessing pass hides the turns it replaces, so a minute of
    far-field Dutch ("ik moet niet zeggen", "zelfs op de vorm van 60-70 minuten") was
    left carrying no *visible* turn at all — and the transcript veto, which counts
    visible turns, saw an empty minute. Bookkeeping about a transcript is not evidence
    about audio. The detector hears the audio.
    """
    store, ids = _store(13)

    def vad(path: Path) -> list[SpeechRegion]:
        # One segment holds quiet speech. Nothing about its volume says so.
        if path.name == "seg006.opus":
            return [SpeechRegion(start=12.0, end=20.2)]
        return []

    assert analyse_segments(store, vad=vad) == 13

    spans = quiet_spans(store, min_duration_s=300.0)
    swept = {int(a) for span in spans for a in span.audio_ids}
    assert ids[6] not in swept  # the speech survives...
    assert len(spans) == 2  # ...and it splits the quiet either side of it


def test_a_segment_nobody_has_listened_to_is_never_swept() -> None:
    # Unheard is unknown, and unknown is never the safest thing to delete. The span only
    # appears once the detector has cleared its audio.
    store, _ = _store(10)
    assert quiet_spans(store, min_duration_s=300.0) == []

    analyse_segments(store, vad=_silent)
    spans = quiet_spans(store, min_duration_s=300.0)
    assert len(spans) == 1
    assert len(spans[0].audio_ids) == 10


def test_the_detector_is_not_run_twice_on_the_same_segment() -> None:
    # ~0.6s of model per minute of audio: it is cached per segment and never paid twice.
    store, _ = _store(6)
    heard: list[str] = []

    def counting(path: Path) -> list[SpeechRegion]:
        heard.append(path.name)
        return []

    assert analyse_segments(store, vad=counting) == 6
    assert analyse_segments(store, vad=counting) == 0
    assert len(heard) == 6


def test_a_loud_segment_is_still_handed_to_the_detector() -> None:
    """Volume is not a reason to skip a segment, and used to be.

    The detector's queue was once filtered to `mean_volume <= -60` — "a loud segment
    cannot enter a span, so listening to it would spend model time on a foregone
    conclusion". The conclusion was not foregone. On this mic a *bump* averages -56 dB,
    a door -54, and the minute holds no speech whatever; but nobody listened, so it
    stayed unknown for ever, and an unknown minute breaks a run. Twelve hours of the
    archive sat unheard in that band, quietly cutting hour-long silences into shards.
    """
    store, ids = _store(4)
    store.set_audio_measurement(
        AudioSegmentId(ids[2]), -30.0, b"\x00\x00"
    )  # a bump, a door, someone talking — the detector says which, not the meter
    heard: list[str] = []

    def counting(path: Path) -> list[SpeechRegion]:
        heard.append(path.name)
        return []

    analyse_segments(store, vad=counting)
    assert "seg002.opus" in heard
    assert len(heard) == 4


def test_a_segment_bearing_a_turn_is_never_handed_to_the_detector() -> None:
    # The one filter left, and it is free of volume: a segment still showing a turn is
    # already vetoed by the transcript, so the detector's verdict could not change
    # anything. That is a foregone conclusion — this is what one actually looks like.
    store, ids = _store(4)
    store.add_transcript_segment(
        audio_segment_id=int(ids[2]),
        start=BASE,
        end=BASE + timedelta(seconds=4),
        text="dat is een goed idee",
        asr_model="whisper",
    )
    heard: list[str] = []

    def counting(path: Path) -> list[SpeechRegion]:
        heard.append(path.name)
        return []

    analyse_segments(store, vad=counting)
    assert "seg002.opus" not in heard
    assert len(heard) == 3


def test_analysis_records_what_it_heard(monkeypatch: pytest.MonkeyPatch) -> None:
    store, ids = _store(3)

    def vad(path: Path) -> list[SpeechRegion]:
        if path.name == "seg001.opus":
            return [SpeechRegion(0.0, 1.5), SpeechRegion(4.0, 6.5)]
        return []

    analyse_segments(store, vad=vad)
    volumes = {
        int(v.audio_id): v for v in store.audio_segment_volumes(kinds=SWEEPABLE_KINDS)
    }
    assert volumes[ids[1]].speech_s == pytest.approx(4.0)  # 1.5 + 2.5 seconds
    assert volumes[ids[0]].speech_s == 0.0
