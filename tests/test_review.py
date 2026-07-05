"""Correction logic: supersede + record training pair, and the review queue."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta

import pytest

from recall.review import (
    HUMAN_MODEL,
    SpeakerFragment,
    apply_correction,
    review_queue,
    split_correction,
)
from recall.sources import AudioSource, SourceKind
from recall.store import Store
from recall.timeline import Segment

BASE = datetime(2026, 6, 13, 12, 0, 0, tzinfo=UTC)
NOW = datetime(2026, 6, 13, 16, 0, 0, tzinfo=UTC)


def _usb() -> AudioSource:
    return AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")


def _store_with_segment(text: str, *, confidence: float | None) -> tuple[Store, int]:
    store = Store.memory()
    store.add_source(_usb())
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
    seg_id = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=2),
        text=text,
        asr_model="whisper",
        language="en",
        asr_confidence=confidence,
    )
    return store, seg_id


def test_apply_correction_supersedes_and_records_pair() -> None:
    store, seg_id = _store_with_segment("we need more comfort", confidence=0.5)

    new_id = apply_correction(store, seg_id, "we need more coffee", now=NOW)

    # search now finds the corrected text, not the original
    hits = store.search("coffee")
    assert len(hits) == 1
    assert hits[0].id == new_id
    assert hits[0].text == "we need more coffee"
    assert hits[0].asr_model == HUMAN_MODEL
    assert hits[0].asr_confidence == 1.0
    assert store.search("comfort") == []  # old version is superseded
    # the training pair is recorded
    assert store.correction_count() == 1


def test_apply_correction_can_fix_the_language() -> None:
    # Dutch heard as English: correcting the words AND the language tag.
    store, seg_id = _store_with_segment("No, it is not.", confidence=0.5)
    new_id = apply_correction(store, seg_id, "Nee? Deze niet?", now=NOW, language="nl")
    seg = store.get_transcript(new_id)
    assert seg is not None
    assert seg.language == "nl"


def test_apply_correction_keeps_language_when_unspecified() -> None:
    store, seg_id = _store_with_segment("we need more comfort", confidence=0.5)
    new_id = apply_correction(store, seg_id, "we need more coffee", now=NOW)
    seg = store.get_transcript(new_id)
    assert seg is not None
    assert seg.language == "en"  # original kept


def test_split_correction_makes_per_speaker_fragments() -> None:
    store, seg_id = _store_with_segment("hallo daar ja wat anders", confidence=0.5)

    new_ids = split_correction(
        store,
        seg_id,
        [
            SpeakerFragment(
                start=BASE,
                end=BASE + timedelta(seconds=1),
                text="hallo daar",
                speaker="Carol",
            ),
            SpeakerFragment(
                start=BASE + timedelta(seconds=1),
                end=BASE + timedelta(seconds=2),
                text="ja, wat anders",
                speaker="Alice",
            ),
        ],
        now=NOW,
    )
    assert len(new_ids) == 2

    rows = store.segments_in_range(BASE, BASE + timedelta(seconds=10))
    assert [(r.text, r.speaker_label) for r in rows] == [
        ("hallo daar", "Carol"),
        ("ja, wat anders", "Alice"),
    ]
    assert all(r.asr_model == HUMAN_MODEL for r in rows)
    assert store.search("anders")  # original superseded, fragments searchable
    assert store.correction_count() == 2  # two labelled pairs
    assert store.sources_of(new_ids[0]) == [seg_id]  # lineage to the original


def test_apply_correction_strips_and_rejects_blank() -> None:
    store, seg_id = _store_with_segment("x", confidence=0.5)
    with pytest.raises(ValueError, match="blank"):
        apply_correction(store, seg_id, "   ", now=NOW)


def test_apply_correction_unknown_id_raises() -> None:
    store, _ = _store_with_segment("x", confidence=0.5)
    with pytest.raises(ValueError, match="no transcript segment"):
        apply_correction(store, 9999, "text", now=NOW)


def test_review_queue_orders_low_confidence_first() -> None:
    store = Store.memory()
    store.add_source(_usb())
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
    for text, conf, offset in [("high", 0.99, 0), ("low", 0.3, 1), ("mid", 0.7, 2)]:
        store.add_transcript_segment(
            audio_segment_id=audio_id,
            start=BASE + timedelta(seconds=offset),
            end=BASE + timedelta(seconds=offset + 1),
            text=text,
            asr_model="whisper",
            asr_confidence=conf,
        )

    queue = review_queue(store, max_confidence=0.9)
    # only sub-0.9, lowest first; 0.99 excluded
    assert [s.text for s in queue] == ["low", "mid"]


def test_apply_correction_preserves_the_diarization_cluster() -> None:
    # Correcting a turn's text must keep it attributed to its voice, so a named voice
    # in a session doesn't fall back to "unknown" after an edit.
    store = Store.memory()
    store.add_source(_usb())
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
    seg_id = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=2),
        text="raw",
        asr_model="diarized",
        speaker_cluster="SPEAKER_01",
    )
    new_id = apply_correction(store, seg_id, "fixed text", now=NOW)
    corrected = store.get_transcript(new_id)
    assert corrected is not None
    assert corrected.speaker_cluster == "SPEAKER_01"


def test_apply_correction_refuses_an_already_superseded_turn() -> None:
    # A double-tap / second-tab correction of a stale id must not mint a SECOND
    # "current" human turn (duplicate timeline bubbles, duplicate corpus pairs).
    store = Store.memory()
    store.add_source(_usb())
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
    seg_id = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=2),
        text="raw",
        asr_model="whisper",
    )
    apply_correction(store, seg_id, "first correction", now=NOW)
    with pytest.raises(ValueError, match="superseded"):
        apply_correction(store, seg_id, "second correction", now=NOW)
    # Exactly one corpus pair was recorded.
    assert store.correction_count() == 1
