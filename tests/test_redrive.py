"""Re-deriving the archive: supersede old machine turns, preserve human ones."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest

from conftest import make_flac
from recall.asr import AsrResult, AsrSegment
from recall.redrive import redrive_archive
from recall.review import apply_correction
from recall.sources import AudioSource, SourceKind
from recall.store import Store
from recall.timeline import Segment
from recall.vad import SpeechRegion

BASE = datetime(2026, 6, 13, 12, 0, 0, tzinfo=UTC)


def test_redrive_supersedes_machine_but_keeps_human(tmp_path: Path) -> None:
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
    # an old looping machine turn, and one the human has corrected
    loop = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=1),
        text="goog goog goog goog",
        asr_model="old",
    )
    corrected = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=2),
        end=BASE + timedelta(seconds=3),
        text="x",
        asr_model="old",
    )
    apply_correction(store, corrected, "echte tekst", now=BASE)

    def vad(_a: Path) -> list[SpeechRegion]:
        return [SpeechRegion(0.0, 3.0)]

    def transcriber(_a: Path) -> AsrResult:
        return AsrResult(
            language="nl",
            language_confidence=0.9,
            segments=(
                AsrSegment(
                    start=0.0,
                    end=1.0,
                    text="schone tekst",
                    avg_logprob=-0.2,
                    no_speech_prob=0.0,
                ),
                AsrSegment(
                    start=2.0,
                    end=3.0,
                    text="machine over human",
                    avg_logprob=-0.2,
                    no_speech_prob=0.0,
                ),
            ),
        )

    added = redrive_archive(
        store, transcriber, vad, work_dir=tmp_path / "work", model_name="new-v2"
    )
    assert added == 1  # only the non-human-overlapping turn is re-added
    assert list((tmp_path / "work").glob("*.wav")) == []  # scratch self-cleaned

    texts = [s.text for s in store.segments_in_range(BASE, BASE + timedelta(seconds=5))]
    assert "schone tekst" in texts  # re-derived clean turn
    assert "echte tekst" in texts  # human correction preserved
    assert "goog goog goog goog" not in texts  # old loop superseded (hidden)
    assert "machine over human" not in texts  # didn't clobber the human span

    old_loop = store.get_transcript(loop)
    assert old_loop is not None and old_loop.hidden_reason is not None  # kept, hidden


def test_redrive_failure_does_not_blank_the_segment(tmp_path: Path) -> None:
    """A transcriber failure mid-pass must not leave the segment blank.

    redrive must only hide a segment's old machine turns once their replacements
    are ready to be written — exactly as ``refine_diarized`` already does. If it
    hides first and the slow transcribe then fails (ASR crash, OOM, the pass being
    killed mid-segment), the segment is left with no visible turns until some later
    pass re-derives it. Same window a reader hits mid-pass: old hidden, new not yet
    added → an empty recording.
    """
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
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=1),
        text="the original transcript",
        asr_model="old",
    )

    def vad(_a: Path) -> list[SpeechRegion]:
        return [SpeechRegion(0.0, 3.0)]

    def failing_transcriber(_a: Path) -> AsrResult:
        raise RuntimeError("ASR crashed mid-pass")

    with pytest.raises(RuntimeError):
        redrive_archive(
            store,
            failing_transcriber,
            vad,
            work_dir=tmp_path / "work",
            model_name="new-v2",
        )

    # The crash must not have blanked the segment: its original turn is still visible.
    texts = [s.text for s in store.segments_in_range(BASE, BASE + timedelta(seconds=5))]
    assert "the original transcript" in texts


def test_redrive_crash_mid_write_leaves_the_old_turns_visible(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # Ordering hide-after-transcribe (the test above) protects against the heavy
    # step failing — but a crash BETWEEN the hide and the inserts still blanks the
    # segment forever (the hidden turns carry the 'reprocessed' marker, so it is
    # never re-picked). The hide+insert must be one atomic transaction.
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
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=1),
        text="the original transcript",
        asr_model="old",
    )

    def transcriber(_a: Path) -> AsrResult:
        return AsrResult(
            language="en",
            language_confidence=0.9,
            segments=(
                AsrSegment(
                    start=0.0,
                    end=1.0,
                    text="replacement",
                    avg_logprob=-0.2,
                    no_speech_prob=0.0,
                ),
            ),
        )

    def exploding_add(*_args: object, **_kwargs: object) -> int:
        msg = "simulated crash mid-write"
        raise RuntimeError(msg)

    monkeypatch.setattr(Store, "add_transcript_segment", exploding_add)
    with pytest.raises(RuntimeError, match="mid-write"):
        redrive_archive(
            store,
            transcriber,
            lambda _a: [SpeechRegion(0.0, 3.0)],
            work_dir=tmp_path / "work",
            model_name="new-v2",
        )
    monkeypatch.undo()

    texts = [s.text for s in store.segments_in_range(BASE, BASE + timedelta(seconds=5))]
    assert "the original transcript" in texts
