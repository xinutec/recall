"""End-to-end speaker identification on real audio with a stub embedder."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from pathlib import Path

from conftest import make_flac
from recall.identify import (
    backfill_embeddings,
    backfill_voiceprints,
    identify_segments,
    rematch_speaker_guesses,
)
from recall.sources import AudioSource, SourceKind
from recall.store import Store
from recall.timeline import Segment

BASE = datetime(2026, 6, 13, 12, 0, 0, tzinfo=UTC)
NOW = datetime(2026, 6, 13, 16, 0, 0, tzinfo=UTC)


def test_identify_resolves_enrolled_speaker(tmp_path: Path) -> None:
    flac = tmp_path / "usb-20260613T120000.flac"
    make_flac(flac, 3.0)

    store = Store.memory()
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    ann_id = store.enroll_speaker("ann", [1.0, 0.0, 0.0], now=NOW)
    store.enroll_speaker("bob", [0.0, 1.0, 0.0], now=NOW)

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
        end=BASE + timedelta(seconds=1.5),
        text="hallo",
        asr_model="whisper",
        speaker_label="SPEAKER_00",
    )

    # a stub embedder that returns a vector clearly closest to ann
    def stub_embedder(_audio: Path) -> list[float]:
        return [0.95, 0.05, 0.0]

    resolved = identify_segments(
        store, stub_embedder, work_dir=tmp_path / "work", threshold=0.8
    )
    assert resolved == 1

    segment = store.get_transcript(seg_id)
    assert segment is not None
    assert segment.speaker_id == ann_id


def test_identify_cleans_up_its_clips(tmp_path: Path) -> None:
    flac = tmp_path / "usb-20260613T120000.flac"
    make_flac(flac, 3.0)
    store = Store.memory()
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    store.enroll_speaker("ann", [1.0, 0.0, 0.0], now=NOW)
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
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=0.5),
        end=BASE + timedelta(seconds=1.5),
        text="hallo",
        asr_model="whisper",
        speaker_label="SPEAKER_00",
    )

    def stub_embedder(_audio: Path) -> list[float]:
        return [0.95, 0.05, 0.0]

    work = tmp_path / "work"
    identify_segments(store, stub_embedder, work_dir=work, threshold=0.8)
    # The per-turn clip sliced for embedding is removed once matched; work/ stays empty.
    assert list(work.glob("*.wav")) == []


def _seed_audio(store: Store, tmp_path: Path) -> int:
    flac = tmp_path / "usb-20260613T120000.flac"
    make_flac(flac, 3.0)
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    return store.add_audio_segment(
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


def test_backfill_voiceprints_enrolls_from_labelled_turns(tmp_path: Path) -> None:
    # Labelling is enrolment: a current human-labelled turn becomes a voiceprint
    # (covering session-view assigns, not just text edits), and never twice.
    store = Store.memory()
    audio_id = _seed_audio(store, tmp_path)
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=1.5),
        text="x",
        asr_model="diarized",
        speaker_label="Alice",
    )

    def stub(_audio: Path) -> list[float]:
        return [0.1, 0.9, 0.0]

    enrolled = backfill_voiceprints(store, stub, work_dir=tmp_path / "w", now=NOW)
    assert enrolled == 1
    assert "Alice" in store.speaker_profiles()
    # idempotent — the turn is already a voiceprint, so nothing left to do
    assert backfill_voiceprints(store, stub, work_dir=tmp_path / "w", now=NOW) == 0


def test_backfill_voiceprints_skips_short_and_faint_clips(tmp_path: Path) -> None:
    # A sub-second sliver (a one-word split) or near-silent labelled turn keeps its
    # text/label but must NOT enrol a voiceprint — too little voice / mostly noise.
    store = Store.memory()
    audio_id = _seed_audio(store, tmp_path)
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=0.12),  # one-word sliver — too short
        text="you",
        asr_model="diarized",
        speaker_label="Pippijn",
    )
    faint = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=1),
        end=BASE + timedelta(seconds=2.5),
        text="x",
        asr_model="diarized",
        speaker_label="Pippijn",
    )
    store.set_loudness(faint, 0.0)  # near-silent — too faint

    def stub(_audio: Path) -> list[float]:
        return [0.1, 0.9, 0.0]

    assert backfill_voiceprints(store, stub, work_dir=tmp_path / "w", now=NOW) == 0
    assert "Pippijn" not in store.speaker_profiles()


def test_backfill_embeddings_stores_a_vector_once(tmp_path: Path) -> None:
    store = Store.memory()
    audio_id = _seed_audio(store, tmp_path)
    seg = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=1),
        text="x",
        asr_model="whisper",
        asr_confidence=0.5,
    )

    def stub(_audio: Path) -> list[float]:
        return [0.1, 0.95, 0.0]

    assert backfill_embeddings(store, stub, work_dir=tmp_path / "e") == 1
    assert store.embeddings_with_guesses() == [(seg, [0.1, 0.95, 0.0], None, None)]
    # embed once — already embedded, nothing left to do
    assert backfill_embeddings(store, stub, work_dir=tmp_path / "e") == 0


def test_backfill_embeddings_survives_a_bad_clip(tmp_path: Path) -> None:
    # A degenerate clip makes pyannote raise; one bad clip must never crash the
    # always-on worker — it's skipped and the pass continues.
    store = Store.memory()
    audio_id = _seed_audio(store, tmp_path)
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=1),
        text="x",
        asr_model="whisper",
        asr_confidence=0.5,
    )

    def boom(_audio: Path) -> list[float]:
        raise RuntimeError("Kernel size can't be greater than actual input size")

    assert backfill_embeddings(store, boom, work_dir=tmp_path / "e") == 0


def test_rematch_derives_and_refreshes_guesses(tmp_path: Path) -> None:
    # The staleness fix: re-derive a turn's guess from its stored embedding against
    # the *current* voiceprints — and when a new voiceprint makes a different
    # speaker the closer match, the cached guess refreshes, with no re-embedding.
    store = Store.memory()
    audio_id = _seed_audio(store, tmp_path)
    seg = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=1),
        text="x",
        asr_model="whisper",
    )
    store.set_embedding(seg, [0.1, 0.95, 0.0])
    store.enroll_speaker("Alice", [0.0, 1.0, 0.0], now=NOW)
    store.enroll_speaker("Carol", [1.0, 0.0, 0.0], now=NOW)

    assert rematch_speaker_guesses(store) == 1  # one guess written
    s1 = store.get_transcript(seg)
    assert s1 is not None and s1.speaker_guess == "Alice"

    # Nothing changed → no writes (cheap, idempotent).
    assert rematch_speaker_guesses(store) == 0

    # A new Carol voiceprint that matches this clip more closely → guess refreshes.
    store.enroll_speaker("Carol", [0.1, 0.96, 0.0], now=NOW)
    assert rematch_speaker_guesses(store) == 1
    s2 = store.get_transcript(seg)
    assert s2 is not None and s2.speaker_guess == "Carol"


def test_rematch_confidence_is_a_calibrated_softmax(tmp_path: Path) -> None:
    # The score is "how clearly is it this person vs the others", not raw cosine:
    # a clear match reads high; a voice halfway between two people reads ~0.5.
    store = Store.memory()
    audio_id = _seed_audio(store, tmp_path)
    store.enroll_speaker("Alice", [0.0, 1.0, 0.0], now=NOW)
    store.enroll_speaker("Carol", [1.0, 0.0, 0.0], now=NOW)
    clear = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=1),
        text="a",
        asr_model="whisper",
    )
    store.set_embedding(clear, [0.0, 1.0, 0.0])  # exactly Alice
    toss = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=1),
        end=BASE + timedelta(seconds=2),
        text="b",
        asr_model="whisper",
    )
    store.set_embedding(toss, [0.7, 0.7, 0.0])  # equidistant Alice/Carol

    rematch_speaker_guesses(store)
    cs = store.get_transcript(clear)
    ts = store.get_transcript(toss)
    assert cs is not None and cs.speaker_score is not None and cs.speaker_score > 0.9
    assert ts is not None and ts.speaker_score is not None
    assert 0.4 < ts.speaker_score < 0.75  # honestly uncertain


def test_rematch_noop_without_profiles(tmp_path: Path) -> None:
    store = Store.memory()
    audio_id = _seed_audio(store, tmp_path)
    seg = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=1),
        text="x",
        asr_model="whisper",
    )
    store.set_embedding(seg, [1.0, 0.0])
    assert rematch_speaker_guesses(store) == 0  # no enrolled voices to match


def test_identify_leaves_unknown_below_threshold(tmp_path: Path) -> None:
    flac = tmp_path / "usb-20260613T120000.flac"
    make_flac(flac, 3.0)

    store = Store.memory()
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    store.enroll_speaker("ann", [1.0, 0.0, 0.0], now=NOW)
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
        start=BASE,
        end=BASE + timedelta(seconds=1),
        text="hallo",
        asr_model="whisper",
    )

    def stub_embedder(_audio: Path) -> list[float]:
        return [0.0, 0.0, 1.0]  # orthogonal to ann -> unknown

    resolved = identify_segments(
        store, stub_embedder, work_dir=tmp_path / "work", threshold=0.5
    )
    assert resolved == 0
    segment = store.get_transcript(seg_id)
    assert segment is not None
    assert segment.speaker_id is None
