"""Exporting the corrections corpus to a fine-tuning dataset (real audio)."""

from __future__ import annotations

import json
from datetime import UTC, datetime, timedelta
from pathlib import Path

from conftest import make_flac
from recall.review import apply_correction
from recall.sources import AudioSource, SourceKind
from recall.store import Store
from recall.timeline import Segment
from recall.training import export_corpus, split_examples

BASE = datetime(2026, 6, 13, 12, 0, 0, tzinfo=UTC)
NOW = datetime(2026, 6, 13, 16, 0, 0, tzinfo=UTC)


def test_export_corpus_writes_clips_and_manifest(tmp_path: Path) -> None:
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
    seg_id = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=0.5),
        end=BASE + timedelta(seconds=3.0),
        text="we need more comfort",
        asr_model="whisper",
        language="nl",
        asr_confidence=0.5,
    )
    apply_correction(store, seg_id, "we need more coffee", now=NOW)

    dest = tmp_path / "corpus"
    count = export_corpus(store, dest)

    assert count == 1
    lines = (dest / "manifest.jsonl").read_text(encoding="utf-8").splitlines()
    assert len(lines) == 1
    record = json.loads(lines[0])
    assert record["text"] == "we need more coffee"
    assert record["language"] == "nl"
    clip = Path(record["audio"])
    assert clip.exists()
    assert clip.read_bytes()[:4] == b"RIFF"  # a real WAV clip


def test_split_examples_holds_out_a_deterministic_disjoint_fraction() -> None:
    items = list(range(10))
    train, test = split_examples(items, holdout=0.2)

    # ~20% held out, train and test partition the corpus with no overlap.
    assert len(test) == 2
    assert len(train) == 8
    assert set(train) | set(test) == set(items)
    assert not (set(train) & set(test))
    # Deterministic — same input, same split every run.
    assert split_examples(items, holdout=0.2) == (train, test)


def test_split_examples_keeps_train_nonempty_for_tiny_corpora() -> None:
    train, test = split_examples([1, 2, 3], holdout=0.5)
    assert train  # never trains on nothing
    assert test
    assert len(train) + len(test) == 3


def test_export_empty_corpus(tmp_path: Path) -> None:
    store = Store.memory()
    count = export_corpus(store, tmp_path / "corpus")
    assert count == 0
    assert (tmp_path / "corpus" / "manifest.jsonl").read_text() == ""


def _corrected(
    store: Store,
    audio_id: int,
    *,
    start_s: float,
    end_s: float,
    text: str,
) -> None:
    seg_id = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=start_s),
        end=BASE + timedelta(seconds=end_s),
        text="raw",
        asr_model="whisper",
    )
    apply_correction(store, seg_id, text, now=NOW)


def _corpus_store(tmp_path: Path, seconds: float = 30.0) -> tuple[Store, int]:
    flac = tmp_path / "usb-20260613T120000.flac"
    make_flac(flac, seconds)
    store = Store.memory()
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    audio_id = store.add_audio_segment(
        Segment(
            source_id="usb",
            sequence=0,
            start=BASE,
            end=BASE + timedelta(seconds=seconds),
            path=str(flac),
            sample_rate=48000,
            channels=1,
        )
    )
    return store, audio_id


def test_export_corpus_skips_backchannels(tmp_path: Path) -> None:
    # Sub-2s / few-word clips are padded to Whisper's 30s window with silence —
    # a corpus full of them teaches early-EOS (the adapter-truncation regression).
    # Their text stays in the corrections table; they just aren't ASR labels.
    store, audio_id = _corpus_store(tmp_path)
    _corrected(store, audio_id, start_s=0.0, end_s=1.2, text="Yeah. Okay. Good.")
    _corrected(store, audio_id, start_s=2.0, end_s=4.5, text="Yes. No.")
    _corrected(
        store,
        audio_id,
        start_s=5.0,
        end_s=9.0,
        text="the plumber is coming on Thursday at nine",
    )
    count = export_corpus(store, tmp_path / "corpus")
    assert count == 1  # only the long, contentful clip survives


def test_export_corpus_dedupes_overlapping_duplicates(tmp_path: Path) -> None:
    # Overlapping correction spans with the same text (double-labelled from two
    # views) must not weight the corpus twice.
    store, audio_id = _corpus_store(tmp_path)
    _corrected(
        store,
        audio_id,
        start_s=0.0,
        end_s=4.0,
        text="so that was the thermometer then",
    )
    _corrected(
        store,
        audio_id,
        start_s=0.5,
        end_s=4.5,
        text="So that was the thermometer, then.",
    )
    _corrected(
        store,
        audio_id,
        start_s=10.0,
        end_s=14.0,
        text="a completely different sentence entirely here",
    )
    count = export_corpus(store, tmp_path / "corpus")
    assert count == 2  # duplicate folded, distinct span kept
