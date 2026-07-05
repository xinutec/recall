"""The VAD-as-labelling-function hallucination scan (data-improvement pass).

A turn is hidden only when two signals agree: VAD-silence AND repeated-filler text,
so novel real speech (e.g. a one-off quiet utterance the VAD missed) is protected.
"""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from pathlib import Path

from recall.cleanup import HALLUCINATION_REASON, scan_hallucinations
from recall.ids import TranscriptId
from recall.sources import AudioSource, SourceKind
from recall.store import Store
from recall.timeline import Segment
from recall.vad import SpeechRegion, overlaps_speech

BASE = datetime(2026, 6, 13, 12, 0, 0, tzinfo=UTC)


def test_overlaps_speech() -> None:
    regions = [SpeechRegion(2.0, 4.0)]
    assert overlaps_speech(3.0, 5.0, regions) is True
    assert overlaps_speech(1.0, 2.5, regions) is True
    assert overlaps_speech(4.0, 6.0, regions) is False  # touches edge, no overlap
    assert overlaps_speech(0.0, 1.0, regions) is False
    assert overlaps_speech(0.0, 1.0, []) is False


def _store_with_turns(tmp_path: Path) -> tuple[Store, dict[str, TranscriptId]]:
    audio_file = tmp_path / "usb-20260613T120000.opus"
    audio_file.write_bytes(b"")  # the fake VAD ignores contents; only existence matters
    store = Store.memory()
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    audio_id = store.add_audio_segment(
        Segment(
            source_id="usb",
            sequence=0,
            start=BASE,
            end=BASE + timedelta(seconds=60),
            path=str(audio_file),
            sample_rate=48000,
            channels=1,
        )
    )
    ids = {}
    # a filler turn over silence (10-12s), a turn over speech (30-32s), a human turn
    ids["silent"] = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=10),
        end=BASE + timedelta(seconds=12),
        text="Gracias.",
        asr_model="v1",
    )
    ids["speech"] = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=30),
        end=BASE + timedelta(seconds=32),
        text="hallo daar",
        asr_model="v1",
    )
    ids["human"] = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=11),
        end=BASE + timedelta(seconds=12),
        text="real",
        asr_model="human",
    )
    return store, ids


def test_scan_hides_silence_filler_only(tmp_path: Path) -> None:
    store, ids = _store_with_turns(tmp_path)

    def fake_vad(_audio: Path) -> list[SpeechRegion]:
        return [SpeechRegion(29.0, 33.0)]  # speech only around the 30-32s turn

    # min_filler_count=1 makes the single "Gracias." count as filler for the test
    result = scan_hallucinations(store, fake_vad, min_filler_count=1)
    assert result.turns_hidden == 1

    silent = store.get_transcript(ids["silent"])
    speech = store.get_transcript(ids["speech"])
    human = store.get_transcript(ids["human"])
    assert silent is not None and silent.hidden_reason == HALLUCINATION_REASON
    assert speech is not None and speech.hidden_reason is None  # over speech, kept
    assert human is not None and human.hidden_reason is None  # human never touched

    visible = store.segments_in_range(BASE, BASE + timedelta(seconds=60))
    assert ids["silent"] not in [s.id for s in visible]
    assert ids["speech"] in [s.id for s in visible]


def test_scan_protects_novel_text_even_in_silence(tmp_path: Path) -> None:
    store, ids = _store_with_turns(tmp_path)

    def all_silence(_audio: Path) -> list[SpeechRegion]:
        return []

    # default min_filler_count: every text here is a one-off, so none is filler
    result = scan_hallucinations(store, all_silence)
    assert result.turns_hidden == 0
    assert store.get_transcript(ids["silent"]).hidden_reason is None  # type: ignore[union-attr]


def test_scan_pad_keeps_filler_near_speech(tmp_path: Path) -> None:
    store, ids = _store_with_turns(tmp_path)

    # the 10-12s filler turn is within the 1s pad of the 12.6s region → kept;
    # the 30-32s turn has its own region so it isn't hidden either
    def fake_vad(_audio: Path) -> list[SpeechRegion]:
        return [SpeechRegion(12.6, 14.0), SpeechRegion(29.0, 33.0)]

    result = scan_hallucinations(store, fake_vad, min_filler_count=1, pad_s=1.0)
    assert result.turns_hidden == 0
    assert store.get_transcript(ids["silent"]).hidden_reason is None  # type: ignore[union-attr]


def test_scan_is_idempotent(tmp_path: Path) -> None:
    store, _ = _store_with_turns(tmp_path)

    def all_silence(_audio: Path) -> list[SpeechRegion]:
        return []

    first = scan_hallucinations(store, all_silence, min_filler_count=1)
    second = scan_hallucinations(store, all_silence, min_filler_count=1)
    assert first.turns_hidden == 2  # both machine turns are filler-in-silence
    assert second.turns_hidden == 0  # already hidden
