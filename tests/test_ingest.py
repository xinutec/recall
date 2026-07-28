"""End-to-end ingestion on real audio with a fake transcriber (no model).

Generates a real FLAC segment, derives the working copy with real ffmpeg, runs a
stub transcriber, writes transcript segments to the store, and searches them —
proving the whole wiring minus the ML model itself.
"""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from pathlib import Path

from conftest import make_flac, sequential
from recall.asr import AsrResult, AsrSegment
from recall.diarize import Diarization, SpeakerTurn
from recall.ingest import ingest_diarized, ingest_transcripts
from recall.sources import AudioSource, SourceKind
from recall.store import Store
from recall.timeline import Segment
from recall.vad import SpeechRegion

BASE = datetime(2026, 6, 13, 12, 0, 0, tzinfo=UTC)


def test_ingest_writes_searchable_transcripts(tmp_path: Path) -> None:
    flac = tmp_path / "usb-20260613T120000.flac"
    make_flac(flac, 2.0)
    segment = Segment(
        source_id="usb",
        sequence=0,
        start=BASE,
        end=BASE + timedelta(seconds=2),
        path=str(flac),
        sample_rate=48000,
        channels=1,
    )

    def fake_transcriber(_audio: Path) -> AsrResult:
        return AsrResult(
            language="en",
            language_confidence=0.95,
            segments=(
                AsrSegment(
                    start=0.0,
                    end=2.0,
                    text="we need more coffee",
                    avg_logprob=-0.2,
                    no_speech_prob=0.0,
                ),
            ),
        )

    store = Store.memory()
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    count = ingest_transcripts(
        store,
        [segment],
        fake_transcriber,
        work_dir=tmp_path / "work",
        model_name="fake-v1",
    )
    assert count == 1

    results = store.search("coffee")
    assert len(results) == 1
    assert results[0].text == "we need more coffee"
    assert results[0].language == "en"
    assert results[0].start == BASE  # absolute time = segment start + 0s offset
    assert results[0].asr_model == "fake-v1"


def test_ingest_cleans_up_its_working_copy(tmp_path: Path) -> None:
    flac = tmp_path / "usb-20260613T120000.flac"
    make_flac(flac, 2.0)
    segment = Segment(
        source_id="usb",
        sequence=0,
        start=BASE,
        end=BASE + timedelta(seconds=2),
        path=str(flac),
        sample_rate=48000,
        channels=1,
    )

    def fake_transcriber(_audio: Path) -> AsrResult:
        return AsrResult(
            language="en",
            language_confidence=0.9,
            segments=(
                AsrSegment(
                    start=0.0,
                    end=2.0,
                    text="hello",
                    avg_logprob=-0.2,
                    no_speech_prob=0.0,
                ),
            ),
        )

    store = Store.memory()
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    work = tmp_path / "work"
    ingest_transcripts(
        store, [segment], fake_transcriber, work_dir=work, model_name="fake-v1"
    )
    # The decoded working copy is deleted once transcribed; work/ stays empty.
    assert list(work.glob("*.wav")) == []


def test_vad_gate_skips_silence_but_marks_processed(tmp_path: Path) -> None:
    flac = tmp_path / "usb-20260613T120000.flac"
    make_flac(flac, 2.0)
    segment = Segment(
        source_id="usb",
        sequence=0,
        start=BASE,
        end=BASE + timedelta(seconds=2),
        path=str(flac),
        sample_rate=48000,
        channels=1,
    )

    def silent_vad(_audio: Path) -> list[SpeechRegion]:
        return []  # no speech detected anywhere in the segment

    # The whole segment is still transcribed (context), but every turn falls in
    # VAD silence, so all are dropped.
    def fake_transcriber(_audio: Path) -> AsrResult:
        return AsrResult(
            language="en",
            language_confidence=0.9,
            segments=(
                AsrSegment(
                    start=0.0,
                    end=1.0,
                    text="Thank you.",
                    avg_logprob=-0.2,
                    no_speech_prob=0.0,
                ),
            ),
        )

    store = Store.memory()
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    written = ingest_transcripts(
        store,
        [segment],
        fake_transcriber,
        work_dir=tmp_path / "work",
        model_name="fake-v1",
        vad=silent_vad,
    )
    assert written == 0
    # the segment is recorded and marked processed, so it won't be retried
    assert len(store.audio_segment_paths()) == 1
    assert store.pending_audio_segments() == []


def test_vad_filters_full_segment_turns_to_speech(tmp_path: Path) -> None:
    flac = tmp_path / "usb-20260613T120000.flac"
    make_flac(flac, 3.0)
    segment = Segment(
        source_id="usb",
        sequence=0,
        start=BASE,
        end=BASE + timedelta(seconds=3),
        path=str(flac),
        sample_rate=48000,
        channels=1,
    )

    # speech only in the first second; the 2nd turn lands in silence
    def speech_vad(_audio: Path) -> list[SpeechRegion]:
        return [SpeechRegion(0.0, 1.0)]

    def fake_transcriber(_audio: Path) -> AsrResult:
        return AsrResult(
            language="nl",
            language_confidence=0.9,
            segments=(
                AsrSegment(
                    start=0.0,
                    end=1.0,
                    text="hallo daar",
                    avg_logprob=-0.2,
                    no_speech_prob=0.0,
                ),
                AsrSegment(  # falls in VAD silence -> dropped
                    start=2.0,
                    end=2.5,
                    text="Gracias.",
                    avg_logprob=-0.2,
                    no_speech_prob=0.0,
                ),
            ),
        )

    store = Store.memory()
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    written = ingest_transcripts(
        store,
        [segment],
        fake_transcriber,
        work_dir=tmp_path / "work",
        model_name="fake-v1",
        vad=speech_vad,
    )
    assert written == 1  # only the speech-overlapping turn survives
    rows = store.segments_in_range(BASE, BASE + timedelta(seconds=10))
    assert [r.text for r in rows] == ["hallo daar"]
    assert rows[0].provenance == "fake-v1"


def test_ingest_diarized_tags_speaker_turns(tmp_path: Path) -> None:
    flac = tmp_path / "usb-20260613T120000.flac"
    make_flac(flac, 3.0)
    segment = Segment(
        source_id="usb",
        sequence=0,
        start=BASE,
        end=BASE + timedelta(seconds=3),
        path=str(flac),
        sample_rate=48000,
        channels=1,
    )

    def fake_diarizer(_audio: Path) -> Diarization:
        return sequential(
            SpeakerTurn(speaker="SPEAKER_00", start=0.0, end=1.5),
            SpeakerTurn(speaker="SPEAKER_01", start=1.5, end=3.0),
        )

    def fake_transcriber(_audio: Path) -> AsrResult:
        return AsrResult(
            language="en",
            language_confidence=0.9,
            segments=(
                AsrSegment(
                    start=0.0,
                    end=1.0,
                    text="hello there",
                    avg_logprob=-0.2,
                    no_speech_prob=0.0,
                ),
            ),
        )

    store = Store.memory()
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    written = ingest_diarized(
        store,
        [segment],
        fake_diarizer,
        fake_transcriber,
        work_dir=tmp_path / "work",
        model_name="fake-v1",
    )
    assert written == 2

    rows = store.segments_in_range(BASE, BASE + timedelta(seconds=10))
    # The diarizer's relative label is the cluster (a voice), stored in speaker_cluster
    # — the same shape refine produces. speaker_label stays null: a raw 'SPEAKER_nn' is
    # not a confirmed person, and putting it there reads as one in the UI.
    assert [r.speaker_cluster for r in rows] == ["SPEAKER_00", "SPEAKER_01"]
    assert [r.speaker_label for r in rows] == [None, None]
    assert rows[0].start == BASE
    assert rows[1].start == BASE + timedelta(seconds=1.5)
    assert rows[1].language == "en"
