"""Reprocessing: re-transcribe machine output, never touch human corrections."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from pathlib import Path

from conftest import make_flac
from recall.asr import AsrResult, AsrSegment
from recall.reprocess import reprocess
from recall.sources import AudioSource, SourceKind
from recall.store import Store
from recall.timeline import Segment

BASE = datetime(2026, 6, 13, 12, 0, 0, tzinfo=UTC)


def _better(_audio: Path) -> AsrResult:
    return AsrResult(
        language="nl",
        language_confidence=0.99,
        segments=(
            AsrSegment(
                start=0.0,
                end=1.0,
                text="better text",
                avg_logprob=-0.05,
                no_speech_prob=0.0,
            ),
        ),
    )


def _store_with_machine_and_human(tmp_path: Path) -> tuple[Store, int, int]:
    flac = tmp_path / "usb-20260613T120000.flac"
    make_flac(flac, 3.0)
    store = Store.memory()
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    audio_id = store.add_audio_segment(
        Segment(
            source_id="usb",
            sequence=0,
            start=BASE,
            end=BASE + timedelta(seconds=3),
            path=str(flac),
            sample_rate=48000,
            channels=1,
        )
    )
    machine = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=0.5),
        end=BASE + timedelta(seconds=1.5),
        text="old guess",
        asr_model="whisper",
        asr_confidence=0.4,
        speaker_label="SPEAKER_00",
    )
    human = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=1.5),
        end=BASE + timedelta(seconds=2.5),
        text="ground truth",
        asr_model="human",
        asr_confidence=1.0,
    )
    return store, machine, human


def test_reprocess_improves_machine_and_preserves_human(tmp_path: Path) -> None:
    store, _machine, human = _store_with_machine_and_human(tmp_path)

    count = reprocess(
        store, _better, work_dir=tmp_path / "work", model_name="whisper-finetuned"
    )
    assert count == 1  # only the machine-authored segment
    assert list((tmp_path / "work").glob("*.wav")) == []  # scratch self-cleaned

    rows = store.segments_in_range(BASE, BASE + timedelta(seconds=10))
    texts = {r.text for r in rows}
    assert "better text" in texts  # machine segment re-transcribed
    assert "ground truth" in texts  # human correction preserved
    assert "old guess" not in texts  # old machine version superseded

    # the human segment is still current (never superseded)
    human_row = store.get_transcript(human)
    assert human_row is not None
    assert human_row.superseded_by is None

    # the new machine segment carries the new model + the original speaker label
    new = next(r for r in rows if r.text == "better text")
    assert new.asr_model == "whisper-finetuned"
    assert new.speaker_label == "SPEAKER_00"
    assert new.language == "nl"


def test_reprocess_respects_confidence_filter(tmp_path: Path) -> None:
    store, _machine, _human = _store_with_machine_and_human(tmp_path)
    # machine seg has confidence 0.4; a 0.3 ceiling excludes it
    count = reprocess(
        store, _better, work_dir=tmp_path / "work", model_name="x", max_confidence=0.3
    )
    assert count == 0


def test_reprocess_does_not_degrade_to_lower_confidence(tmp_path: Path) -> None:
    store, machine, _human = _store_with_machine_and_human(tmp_path)

    def _worse(_audio: Path) -> AsrResult:
        # very low confidence (avg_logprob -> ~0.05) "hallucination"
        return AsrResult(
            language="en",
            language_confidence=0.2,
            segments=(
                AsrSegment(
                    start=0.0,
                    end=1.0,
                    text="thank you",
                    avg_logprob=-3.0,
                    no_speech_prob=0.0,
                ),
            ),
        )

    count = reprocess(store, _worse, work_dir=tmp_path / "work", model_name="x")
    assert count == 0
    # the original (more confident) machine segment is kept, not superseded
    original = store.get_transcript(machine)
    assert original is not None
    assert original.text == "old guess"
    assert original.superseded_by is None
