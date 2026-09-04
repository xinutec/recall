"""The VAD-as-labelling-function hallucination scan (data-improvement pass).

A turn is hidden only when two signals agree: VAD-silence AND repeated-filler text,
so novel real speech (e.g. a one-off quiet utterance the VAD missed) is protected.
"""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from pathlib import Path

from recall.cleanup import (
    EMPTY_TEXT_REASON,
    FOREIGN_SCRIPT_REASON,
    HALLUCINATION_REASON,
    scan_empty_text,
    scan_foreign_script,
    scan_hallucinations,
)
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


def _store_with_junk(tmp_path: Path) -> tuple[Store, dict[str, TranscriptId]]:
    """A segment carrying the two junk shapes plus the real speech they resemble."""
    audio_file = tmp_path / "usb-20260613T130000.opus"
    audio_file.write_bytes(b"")
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

    def turn(name: str, text: str, at: int, model: str = "v1") -> None:
        ids[name] = store.add_transcript_segment(
            audio_segment_id=audio_id,
            start=BASE + timedelta(seconds=at),
            end=BASE + timedelta(seconds=at + 2),
            text=text,
            asr_model=model,
        )

    ids: dict[str, TranscriptId] = {}
    # Non-Latin script over SILENCE — the shape Whisper emits into an empty room.
    turn("japanese_silent", "おやすみなさい", 10)
    turn("cyrillic_silent", "лавлав", 14)
    # Non-Latin script over REAL SPEECH — a guest, or a mis-transcription of one.
    # Hiding this would erase the fact that somebody spoke.
    turn("japanese_speech", "おやすみなさい", 30)
    # Punctuation with no word in it, over speech: still nothing to lose.
    turn("punctuation", "...", 32)
    turn("asterisks", "***", 34)
    # The quiet real speech a confidence rule would have eaten. Latin script.
    turn("yeah", "Yeah.", 36)
    turn("dutch", "Ja.", 38)
    # A human turn is never touched, whatever it says.
    turn("human_japanese", "おやすみなさい", 12, model="human")
    return store, ids


def _speech_after_29s(_audio: Path) -> list[SpeechRegion]:
    return [SpeechRegion(29.0, 45.0)]


class TestForeignScriptScan:
    """Non-Latin script in a Dutch/English household is a hallucination — but only
    the module's two-signal rule makes that safe to act on, because a visitor really
    can speak another language."""

    def test_hides_non_latin_script_only_where_there_is_no_speech(
        self, tmp_path: Path
    ) -> None:
        store, ids = _store_with_junk(tmp_path)
        hidden = scan_foreign_script(store, _speech_after_29s)
        assert hidden == 2  # the two silent ones, not the one over speech

        for key in ("japanese_silent", "cyrillic_silent"):
            turn = store.get_transcript(ids[key])
            assert turn is not None
            assert turn.hidden_reason == FOREIGN_SCRIPT_REASON

    def test_keeps_non_latin_script_over_real_speech(self, tmp_path: Path) -> None:
        """A guest speaking Japanese is content, not noise."""
        store, ids = _store_with_junk(tmp_path)
        scan_foreign_script(store, _speech_after_29s)
        turn = store.get_transcript(ids["japanese_speech"])
        assert turn is not None and turn.hidden_reason is None

    def test_never_touches_latin_script_however_quiet(self, tmp_path: Path) -> None:
        """`Yeah.` and `Ja.` are the commonest low-confidence turns in the archive
        and they are real. Script is the signal precisely so these survive."""
        store, ids = _store_with_junk(tmp_path)

        def all_silence(_audio: Path) -> list[SpeechRegion]:
            return []

        scan_foreign_script(store, all_silence)
        for key in ("yeah", "dutch"):
            turn = store.get_transcript(ids[key])
            assert turn is not None and turn.hidden_reason is None

    def test_never_touches_human_turns(self, tmp_path: Path) -> None:
        store, ids = _store_with_junk(tmp_path)

        def all_silence(_audio: Path) -> list[SpeechRegion]:
            return []

        scan_foreign_script(store, all_silence)
        turn = store.get_transcript(ids["human_japanese"])
        assert turn is not None and turn.hidden_reason is None


class TestEmptyTextScan:
    """Punctuation-only turns need no second signal: there is no content to lose,
    whether or not somebody was speaking at the time."""

    def test_hides_turns_with_no_word_in_them(self, tmp_path: Path) -> None:
        store, ids = _store_with_junk(tmp_path)
        hidden = scan_empty_text(store)
        assert hidden == 2
        for key in ("punctuation", "asterisks"):
            turn = store.get_transcript(ids[key])
            assert turn is not None and turn.hidden_reason == EMPTY_TEXT_REASON

    def test_keeps_anything_containing_a_word(self, tmp_path: Path) -> None:
        store, ids = _store_with_junk(tmp_path)
        scan_empty_text(store)
        for key in ("yeah", "dutch", "japanese_speech"):
            turn = store.get_transcript(ids[key])
            assert turn is not None and turn.hidden_reason is None

    def test_needs_no_audio_so_it_cannot_compete_with_capture(
        self, tmp_path: Path
    ) -> None:
        """Pure text, like scan_loops: instant and capture-safe."""
        store, _ = _store_with_junk(tmp_path)
        (tmp_path / "usb-20260613T130000.opus").unlink()
        assert scan_empty_text(store) == 2
