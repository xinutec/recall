"""Diarized refinement: transcribe the whole segment once, then split it by speaker
via word-level alignment — keeping the full-context language/text, attributing each
turn, and preserving human corrections."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest

from conftest import make_flac, sequential
from recall.asr import AsrResult, AsrSegment, Word
from recall.diarize import Diarization, SpeakerTurn
from recall.refine import refine_diarized
from recall.review import apply_correction
from recall.sources import AudioSource, SourceKind
from recall.store import Store
from recall.timeline import Segment

BASE = datetime(2026, 6, 19, 13, 0, 0, tzinfo=UTC)


def _seg(store: Store, flac: Path, seconds: float = 4.0) -> int:
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    return store.add_audio_segment(
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


def _result(language: str, *words: tuple[float, float, str]) -> AsrResult:
    """A whole-segment transcription with word timings (what `words=True` returns)."""
    ws = tuple(Word(start=s, end=e, text=t, probability=0.9) for s, e, t in words)
    seg = AsrSegment(
        start=ws[0].start,
        end=ws[-1].end,
        text="".join(w.text for w in ws),
        avg_logprob=-0.2,
        no_speech_prob=0.0,
        words=ws,
    )
    return AsrResult(language=language, language_confidence=0.9, segments=(seg,))


# Stub embedder: every clip embeds to the same vector, so a matching enrolled
# voiceprint scores a perfect cosine. Tests that enrol no one just ignore it.
def _embed(_clip: Path) -> list[float]:
    return [1.0, 0.0, 0.0]


def test_refine_splits_by_speaker_via_word_alignment(tmp_path: Path) -> None:
    flac = tmp_path / "usb-20260619T130000.flac"
    make_flac(flac, 4.0)
    store = Store.memory()
    audio_id = _seg(store, flac)
    merged = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=4),
        text="merged basic turn",
        asr_model="old",
    )
    # the whole segment is transcribed once (full context → reliable language)…
    result = _result(
        "en",
        (0.0, 0.5, " can"),
        (0.5, 1.0, " you"),
        (1.6, 2.0, " 29"),
        (2.0, 2.5, " april"),
        (3.0, 3.5, " perfect"),
    )

    def diarizer(_a: Path) -> Diarization:
        return sequential(
            SpeakerTurn(speaker="SPEAKER_00", start=0.0, end=1.2),
            SpeakerTurn(speaker="SPEAKER_01", start=1.2, end=2.8),
            SpeakerTurn(speaker="SPEAKER_00", start=2.8, end=4.0),
        )

    added = refine_diarized(
        store,
        diarizer,
        lambda _a: result,
        _embed,
        work_dir=tmp_path / "work",
        model_name="diarized-v1",
    )
    assert added == 3  # …then split into per-speaker turns by word timing

    by_text = {
        s.text: s for s in store.segments_in_range(BASE, BASE + timedelta(seconds=5))
    }
    assert "can you" in by_text
    assert "29 april" in by_text  # speaker 01's words, grouped at word granularity
    assert "perfect" in by_text
    # language comes from the whole-segment transcription, not a 1s fragment
    assert by_text["29 april"].language == "en"
    assert by_text["29 april"].speaker_label is None
    # the diarization cluster is stored, so the UI can group turns by voice
    assert by_text["29 april"].speaker_cluster == "SPEAKER_01"
    assert by_text["can you"].speaker_cluster == "SPEAKER_00"
    assert (by_text["29 april"].provenance or "").startswith("diarized")
    assert "merged basic turn" not in by_text  # superseded
    old = store.get_transcript(merged)
    assert old is not None and old.hidden_reason is not None
    assert store.audio_segments_to_diarize(limit=10) == []  # marked, not re-picked


def test_refine_cleans_up_its_working_clips(tmp_path: Path) -> None:
    flac = tmp_path / "usb-20260619T130000.flac"
    make_flac(flac, 4.0)
    store = Store.memory()
    audio_id = _seg(store, flac)
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=4),
        text="basic",
        asr_model="old",
    )
    result = _result(
        "en", (0.0, 0.5, " can"), (0.5, 1.0, " you"), (2.0, 2.5, " perfect")
    )

    def diarizer(_a: Path) -> Diarization:
        return sequential(
            SpeakerTurn(speaker="SPEAKER_00", start=0.0, end=1.2),
            SpeakerTurn(speaker="SPEAKER_01", start=1.2, end=4.0),
        )

    work = tmp_path / "work"
    refine_diarized(
        store,
        diarizer,
        lambda _a: result,
        _embed,
        work_dir=work,
        model_name="diarized-v1",
    )
    # The working copy and every per-turn clip are deleted as the pass runs, so the
    # shared work/ dir never grows without bound (see scratch_wav).
    assert list(work.glob("*.wav")) == []


def test_refine_persists_word_timings_rebased_to_the_turn(tmp_path: Path) -> None:
    flac = tmp_path / "usb-20260619T130000.flac"
    make_flac(flac, 4.0)
    store = Store.memory()
    audio_id = _seg(store, flac)
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=4),
        text="basic",
        asr_model="old",
    )
    result = _result("en", (1.6, 2.0, " 29"), (2.0, 2.5, " april"))

    def diarizer(_a: Path) -> Diarization:
        return sequential(SpeakerTurn(speaker="SPEAKER_00", start=0.0, end=4.0))

    refine_diarized(
        store,
        diarizer,
        lambda _a: result,
        _embed,
        work_dir=tmp_path / "work",
        model_name="diarized-v1",
    )
    turn = next(
        s
        for s in store.segments_in_range(BASE, BASE + timedelta(seconds=5))
        if s.text == "29 april"
    )
    assert turn.word_timings is not None
    assert [w.text for w in turn.word_timings] == [" 29", " april"]
    # Re-based to the turn start (the turn begins at the first word, 1.6s in-segment).
    assert turn.word_timings[0].start == 0.0
    assert abs(turn.word_timings[1].end - 0.9) < 1e-6


def test_refine_preserves_human_and_drops_loops(tmp_path: Path) -> None:
    flac = tmp_path / "usb-20260619T130000.flac"
    make_flac(flac, 4.0)
    store = Store.memory()
    audio_id = _seg(store, flac)
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=4),
        text="basic",
        asr_model="old",
    )
    corrected = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=2),
        end=BASE + timedelta(seconds=3),
        text="x",
        asr_model="old",
    )
    apply_correction(store, corrected, "human truth", now=BASE)

    result = _result(
        "nl",
        (0.0, 1.0, " hallo"),  # speaker 00
        (2.0, 3.0, " machine"),  # speaker 01, over the human span
        (3.0, 3.2, " grey"),
        (3.2, 3.4, " grey"),
        (3.4, 3.6, " grey"),
        (3.6, 3.8, " grey"),
        (3.8, 4.0, " grey"),  # speaker 00, a loop
    )

    def diarizer(_a: Path) -> Diarization:
        return sequential(
            SpeakerTurn(speaker="SPEAKER_00", start=0.0, end=1.5),
            SpeakerTurn(speaker="SPEAKER_01", start=1.5, end=2.8),  # human span
            SpeakerTurn(speaker="SPEAKER_00", start=2.8, end=4.0),  # the loop
        )

    added = refine_diarized(
        store,
        diarizer,
        lambda _a: result,
        _embed,
        work_dir=tmp_path / "work",
        model_name="diarized-v1",
    )
    assert added == 1  # only "hallo" — human span skipped, loop dropped

    texts = [s.text for s in store.segments_in_range(BASE, BASE + timedelta(seconds=5))]
    assert "hallo" in texts
    assert "human truth" in texts  # ground truth preserved
    assert "machine" not in texts  # didn't clobber the human span
    assert not any("grey grey" in t for t in texts)  # loop dropped


def test_refine_attributes_turns_to_enrolled_voices(tmp_path: Path) -> None:
    flac = tmp_path / "usb-20260619T130000.flac"
    make_flac(flac, 4.0)
    store = Store.memory()
    audio_id = _seg(store, flac)
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=4),
        text="basic",
        asr_model="old",
    )
    store.enroll_speaker("Alice", [1.0, 0.0, 0.0], now=BASE)  # matches _embed

    result = _result("nl", (0.0, 0.5, " dit"), (0.5, 1.0, " is"), (1.0, 2.0, " alice"))

    def diarizer(_a: Path) -> Diarization:
        return sequential(SpeakerTurn(speaker="SPEAKER_00", start=0.0, end=2.0))

    refine_diarized(
        store,
        diarizer,
        lambda _a: result,
        _embed,
        work_dir=tmp_path / "work",
        model_name="diarized-v1",
    )

    turn = next(
        s
        for s in store.segments_in_range(BASE, BASE + timedelta(seconds=5))
        if s.text == "dit is alice"
    )
    assert turn.speaker_guess == "Alice"  # attributed immediately, no worker wait


def test_refine_redo_upgrades_older_pipeline_turns(tmp_path: Path) -> None:
    flac = tmp_path / "usb-20260619T130000.flac"
    make_flac(flac, 4.0)
    store = Store.memory()
    audio_id = _seg(store, flac)
    # an older-pipeline diarized segment: a hidden basic turn (the 'diarized' marker)
    # and a visible diarized turn whose provenance predates the aligned marker.
    basic = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=2),
        text="basic",
        asr_model="old",
    )
    store.hide(basic, "diarized (old)")
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=2),
        text="oude diarized turn",
        asr_model="old",
        provenance="diarized (old)",
    )
    # it's done (not re-picked by the first pass) but is an upgrade candidate
    assert store.audio_segments_to_diarize(limit=10) == []
    assert store.audio_segments_to_rediarize(limit=10) == [audio_id]

    result = _result("nl", (0.0, 0.5, " dit"), (0.5, 1.0, " is"), (1.0, 2.0, " nieuw"))

    def diarizer(_a: Path) -> Diarization:
        return sequential(SpeakerTurn(speaker="SPEAKER_00", start=0.0, end=2.0))

    added = refine_diarized(
        store,
        diarizer,
        lambda _a: result,
        _embed,
        work_dir=tmp_path / "work",
        model_name="new",
        limit=10,
        redo=True,
    )
    assert added == 1

    by_text = {
        s.text: s for s in store.segments_in_range(BASE, BASE + timedelta(seconds=3))
    }
    assert "dit is nieuw" in by_text
    assert (by_text["dit is nieuw"].provenance or "").startswith("diarized-aligned")
    assert "oude diarized turn" not in by_text  # superseded
    assert store.audio_segments_to_rediarize(limit=10) == []  # upgraded → terminates


def test_refine_flags_non_household_language_as_unreliable(tmp_path: Path) -> None:
    flac = tmp_path / "usb-20260619T130000.flac"
    make_flac(flac, 4.0)
    store = Store.memory()
    audio_id = _seg(store, flac)
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=2),
        text="basic",
        asr_model="old",
    )
    # the whole segment transcribes as Japanese (a hallucination on unclear audio),
    # with high per-word probability — confident, but garbage.
    result = _result("ja", (0.0, 1.0, " ちょっと"), (1.0, 2.0, " 耳を"))

    def diarizer(_a: Path) -> Diarization:
        return sequential(SpeakerTurn(speaker="SPEAKER_00", start=0.0, end=2.0))

    refine_diarized(
        store,
        diarizer,
        lambda _a: result,
        _embed,
        work_dir=tmp_path / "work",
        model_name="diarized-v1",
    )
    turns = [
        s
        for s in store.segments_in_range(BASE, BASE + timedelta(seconds=3))
        if s.language == "ja"
    ]
    assert turns  # kept (with audio), not dropped…
    assert all(t.asr_confidence == 0.0 for t in turns)  # …but flagged unreliable


def test_refine_keeps_old_turns_visible_during_the_heavy_pass(tmp_path: Path) -> None:
    # The diarize pass must not blank a segment while it does the slow transcribe +
    # diarize. Otherwise a reader (a session / the timeline) landing mid-pass sees the
    # old turns hidden but the replacements not yet written — an empty recording.
    flac = tmp_path / "usb-20260619T130000.flac"
    make_flac(flac, 4.0)
    store = Store.memory()
    audio_id = _seg(store, flac)
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=4),
        text="merged basic turn",
        asr_model="old",
    )
    result = _result("en", (0.0, 0.5, " can"), (0.5, 1.0, " you"))

    visible_during_transcribe: list[int] = []

    def spy_transcriber(_a: Path) -> AsrResult:
        # The heavy step. The old turn must still be visible here — its replacement
        # doesn't exist yet, so hiding it now blanks the segment for a mid-pass reader.
        visible_during_transcribe.append(
            len(store.visible_machine_turns_for_audio(audio_id))
        )
        return result

    def diarizer(_a: Path) -> Diarization:
        return sequential(SpeakerTurn(speaker="SPEAKER_00", start=0.0, end=1.0))

    refine_diarized(
        store,
        diarizer,
        spy_transcriber,
        _embed,
        work_dir=tmp_path / "work",
        model_name="diarized-v1",
    )
    assert visible_during_transcribe == [1]  # still visible through the heavy work


def test_refine_crash_mid_write_leaves_the_old_turns_visible(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # A crash between hiding the old turns and inserting the replacements must
    # roll the hide back. Otherwise the segment is blank FOREVER: the hidden turns
    # carry the 'diarized' marker, so no later pass ever re-picks the segment.
    flac = tmp_path / "usb-20260619T130000.flac"
    make_flac(flac, 4.0)
    store = Store.memory()
    audio_id = _seg(store, flac)
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=4),
        text="merged basic turn",
        asr_model="old",
    )
    result = _result("en", (0.0, 0.5, " can"), (0.5, 1.0, " you"))

    def exploding_add(*_args: object, **_kwargs: object) -> int:
        msg = "simulated crash mid-write"
        raise RuntimeError(msg)

    monkeypatch.setattr(Store, "add_transcript_segment", exploding_add)
    with pytest.raises(RuntimeError, match="mid-write"):
        refine_diarized(
            store,
            lambda _a: sequential(
                SpeakerTurn(speaker="SPEAKER_00", start=0.0, end=1.0)
            ),
            lambda _a: result,
            _embed,
            work_dir=tmp_path / "work",
            model_name="diarized-v1",
        )
    monkeypatch.undo()

    turns = store.visible_machine_turns_for_audio(audio_id)
    assert [t.text for t in turns] == ["merged basic turn"]


def test_refine_keeps_a_full_transcript_when_the_pass_covers_too_little(
    tmp_path: Path,
) -> None:
    # A degenerate refined pass — e.g. a long-form decode truncated to its first window,
    # returning a handful of words for a long recording — must NOT replace the full
    # transcript. Swapping it in would hide the whole recording behind a couple of turns
    # (exactly the 34-min meeting that showed 5 turns). The coverage guard keeps the
    # existing turns and skips the swap.
    flac = tmp_path / "usb-20260619T130000.flac"
    make_flac(flac, 4.0)
    store = Store.memory()
    audio_id = _seg(store, flac)
    # >200 chars of real transcript text — enough to be worth protecting
    full = ("the quick brown fox jumps over the lazy dog " * 8).strip()
    kept = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=4),
        text=full,
        asr_model="mlx-community/whisper-large-v3-turbo",
    )
    # the refined pass comes back with almost nothing (the truncation bug's signature)
    result = _result("en", (0.0, 0.5, " yes"))

    def diarizer(_a: Path) -> Diarization:
        return sequential(SpeakerTurn(speaker="SPEAKER_00", start=0.0, end=4.0))

    added = refine_diarized(
        store,
        diarizer,
        lambda _a: result,
        _embed,
        work_dir=tmp_path / "work",
        model_name="adapter",
    )
    assert added == 0  # nothing swapped in
    turns = store.visible_machine_turns_for_audio(audio_id)
    assert [t.id for t in turns] == [kept]  # the full transcript is still visible…
    got = store.get_transcript(kept)
    assert got is not None and got.hidden_reason is None  # …and not hidden
    # …and the guard records a skip so the daemon advances past it instead of
    # re-picking the same newest un-diarized segment every pass (a live-lock while
    # capture is paused — no newer segment ever bumps it out of the "newest" slot).
    assert store.is_diarize_skipped(audio_id)
    assert store.audio_segments_to_diarize(limit=10) == []


def test_guard_skipped_segment_leaves_the_picker_but_a_forced_rederive_still_runs(
    tmp_path: Path,
) -> None:
    # The coverage guard keeps a good transcript by writing nothing — but the segment
    # then still matched `audio_segments_to_diarize` (it never got a 'diarized' marker),
    # so the newest-first, limit-1 daemon re-picked the SAME segment every pass and
    # live-locked, never draining the queue while capture was paused. The guard now
    # records a skip: the auto-picker advances past it, while an explicit forced
    # re-derive still ignores the skip, reprocesses it, and clears it on success.
    flac = tmp_path / "usb-20260619T130000.flac"
    make_flac(flac, 4.0)
    store = Store.memory()
    audio_id = _seg(store, flac)
    full = ("the quick brown fox jumps over the lazy dog " * 8).strip()
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=4),
        text=full,
        asr_model="mlx-community/whisper-large-v3-turbo",
    )

    def diarizer(_a: Path) -> Diarization:
        return sequential(SpeakerTurn(speaker="SPEAKER_00", start=0.0, end=4.0))

    assert store.audio_segments_to_diarize(limit=10) == [audio_id]  # queued to start

    # First pass: the refined transcript is degenerate (a couple of words), guard trips.
    thin = _result("en", (0.0, 0.5, " yes"))
    assert (
        refine_diarized(
            store,
            diarizer,
            lambda _a: thin,
            _embed,
            work_dir=tmp_path / "work",
            model_name="adapter",
        )
        == 0
    )
    # The live-lock is gone: the auto-picker no longer returns the skipped segment.
    assert store.audio_segments_to_diarize(limit=10) == []
    assert store.is_diarize_skipped(audio_id)

    # A forced re-derive of the source ignores the skip and reprocesses it. This pass
    # covers the segment properly (well over half the existing text, all inside the
    # 0-4s diarized window), so it writes the refined turns and clears the skip.
    good = _result(
        "en", *[(i * 0.12, i * 0.12 + 0.06, f" token{i:02d}") for i in range(30)]
    )
    added = refine_diarized(
        store,
        diarizer,
        lambda _a: good,
        _embed,
        work_dir=tmp_path / "work",
        model_name="adapter",
        source="usb",
    )
    assert added == 1  # the forced pass wrote the refined turns…
    assert not store.is_diarize_skipped(audio_id)  # …and cleared the stale skip


def test_refine_survives_a_clip_that_fails_to_slice(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # A corrupt-frame source (an mp3 with a missing header) makes ffmpeg fail when
    # slicing some turns' audio for embedding. That must skip the turn's embedding, not
    # crash the whole pass — otherwise a queued re-diarize request is re-picked forever
    # (the daemon crash-loops, re-transcribing the meeting every restart).
    flac = tmp_path / "usb-20260619T130000.flac"
    make_flac(flac, 4.0)
    store = Store.memory()
    audio_id = _seg(store, flac)
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=4),
        text="basic",
        asr_model="old",
    )
    result = _result("en", (0.0, 0.5, " can"), (0.5, 1.0, " you"), (2.0, 2.5, " sir"))

    def exploding_slice(*_args: object, **_kwargs: object) -> None:
        msg = "simulated ffmpeg slice failure on a corrupt frame"
        raise RuntimeError(msg)

    monkeypatch.setattr("recall.refine.slice_clip", exploding_slice)

    added = refine_diarized(
        store,
        lambda _a: sequential(SpeakerTurn(speaker="SPEAKER_00", start=0.0, end=4.0)),
        lambda _a: result,
        _embed,
        work_dir=tmp_path / "work",
        model_name="diarized-v1",
    )
    # the turns are still written; only the (failed) embeddings are skipped
    assert added == 1
    texts = [s.text for s in store.segments_in_range(BASE, BASE + timedelta(seconds=5))]
    assert "can you sir" in texts


def test_refine_source_redrives_a_finished_segment(tmp_path: Path) -> None:
    # A clean, forced re-derive: targeting a source re-diarizes its segments through the
    # canonical pipeline even when they're already done (not picked by either normal
    # picker) — so one recording can be rebuilt consistently. Human turns are preserved.
    flac = tmp_path / "usb-20260619T130000.flac"
    make_flac(flac, 4.0)
    store = Store.memory()
    audio_id = _seg(store, flac)
    basic = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=2),
        text="basic",
        asr_model="old",
    )
    store.hide(basic, "diarized (old)")
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=2),
        text="already aligned",
        asr_model="old",
        provenance="diarized-aligned (old)",
        speaker_cluster="SPEAKER_00",
    )
    assert store.audio_segments_to_diarize(limit=10) == []  # nothing to do normally

    result = _result("nl", (0.0, 0.5, " dit"), (0.5, 1.0, " is"), (1.0, 2.0, " nieuw"))

    def diarizer(_a: Path) -> Diarization:
        return sequential(SpeakerTurn(speaker="SPEAKER_00", start=0.0, end=2.0))

    added = refine_diarized(
        store,
        diarizer,
        lambda _a: result,
        _embed,
        work_dir=tmp_path / "work",
        model_name="new",
        source="usb",
    )
    assert added == 1
    by_text = {
        s.text: s for s in store.segments_in_range(BASE, BASE + timedelta(seconds=3))
    }
    assert "dit is nieuw" in by_text  # re-derived
    assert by_text["dit is nieuw"].speaker_cluster == "SPEAKER_00"
    assert "already aligned" not in by_text  # the prior version superseded


def test_a_refine_that_produces_nothing_keeps_the_transcript(tmp_path: Path) -> None:
    """A refine replaces a transcript or it keeps it. It never empties one.

    The old code hid the existing turns and *then* applied the filters that drop a turn
    (a repetition loop, a span a human already corrected). A pass whose every turn was
    filtered out — or which produced none at all — therefore hid the transcript and
    wrote
    nothing in its place. It blanked 175 segments of real household conversation that
    way,
    among them a minute of Dutch about writing things down in order to remember them.
    """
    flac = tmp_path / "usb-20260619T130000.flac"
    make_flac(flac, 4.0)
    store = Store.memory()
    audio_id = _seg(store, flac)
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=4),
        text="Schrijf dingen op, zoals die recepten",
        asr_model="old",
    )

    # The diarizer finds no speaker at all, so the pass yields no turns to write.
    result = _result("nl", (0.0, 0.5, " ja"), (0.5, 1.0, " ja"), (1.0, 1.5, " ja"))

    def diarizer(_a: Path) -> Diarization:
        return sequential()

    added = refine_diarized(
        store,
        diarizer,
        lambda _a: result,
        _embed,
        work_dir=tmp_path / "work",
        model_name="diarized-v1",
    )

    assert added == 0
    kept = store.visible_machine_turns_for_audio(audio_id)
    assert [t.text for t in kept] == ["Schrijf dingen op, zoals die recepten"]
