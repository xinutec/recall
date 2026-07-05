"""Align a corrected turn's text to word-level ASR, so human turns get real timings."""

from __future__ import annotations

from collections.abc import Sequence
from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest

from conftest import make_flac
from recall import wordtimings
from recall.asr import AsrResult, AsrSegment, Word, slice_clip
from recall.sources import AudioSource, SourceKind
from recall.store import Store
from recall.timeline import Segment
from recall.wordtimings import align_text_to_audio, backfill_word_timings

BASE = datetime(2026, 6, 13, 12, 0, 0, tzinfo=UTC)


def _w(start: float, end: float, text: str) -> Word:
    return Word(start=start, end=end, text=text, probability=0.9)


def _spans(words: Sequence[Word]) -> list[float]:
    """Flatten [start, end, start, end, …] for exact (approx) timing assertions."""
    return [t for w in words for t in (w.start, w.end)]


def test_aligns_matching_text_to_the_asr_word_times() -> None:
    asr = [_w(0.0, 0.4, " dit"), _w(0.4, 0.8, " is"), _w(1.0, 2.0, " alice")]
    out = align_text_to_audio("dit is alice", asr)
    assert [w.text for w in out] == [" dit", " is", " alice"]
    assert (out[0].start, out[0].end) == (0.0, 0.4)
    assert (out[2].start, out[2].end) == (1.0, 2.0)
    # The words reconstruct the turn text (the format `_recut` reads).
    assert "".join(w.text for w in out).strip() == "dit is alice"


def test_interpolates_a_word_the_human_added() -> None:
    # The human typed "echt", which the ASR never produced — it lands evenly in the gap
    # between its timed neighbours (is → alice), at exactly [1.0, 2.0].
    asr = [_w(0.0, 0.5, " dit"), _w(0.5, 1.0, " is"), _w(2.0, 3.0, " alice")]
    out = align_text_to_audio("dit is echt alice", asr)
    assert [w.text for w in out] == [" dit", " is", " echt", " alice"]
    assert _spans(out) == pytest.approx([0.0, 0.5, 0.5, 1.0, 1.0, 2.0, 2.0, 3.0])


def test_interpolates_a_run_of_added_words_evenly() -> None:
    # Two unmatched words between anchors split the gap evenly — pins the per-word step.
    asr = [_w(0.0, 1.0, " a"), _w(4.0, 5.0, " b")]
    out = align_text_to_audio("a x y b", asr)
    assert [w.text for w in out] == [" a", " x", " y", " b"]
    # gap 1.0→4.0 over two words = 1.5 each: x=[1.0,2.5], y=[2.5,4.0]
    assert _spans(out) == pytest.approx([0.0, 1.0, 1.0, 2.5, 2.5, 4.0, 4.0, 5.0])


def test_no_match_spreads_the_whole_span_evenly() -> None:
    # Nothing matches the ASR at all → the clip's whole span [1.0, 4.0] is divided
    # evenly across the words (the no-anchor branch). A non-zero start makes the +/-
    # in the spread observable.
    asr = [_w(1.0, 4.0, " zzz")]
    out = align_text_to_audio("foo bar baz", asr)
    assert [w.text for w in out] == [" foo", " bar", " baz"]
    assert _spans(out) == pytest.approx([1.0, 2.0, 2.0, 3.0, 3.0, 4.0])


def test_punctuation_and_case_do_not_block_matching() -> None:
    asr = [_w(0.0, 0.5, " mask"), _w(0.6, 1.0, " thank"), _w(1.0, 1.4, " you")]
    out = align_text_to_audio("Mask. Thank you.", asr)
    assert (out[0].start, out[2].end) == (0.0, 1.4)  # matched despite "." and case


def test_a_misheard_word_takes_the_replaced_words_exact_time() -> None:
    # ASR heard "alex"; the human fixed it to "alice". A 1:1 replacement maps
    # positionally — "alice" takes "alex"'s exact span [1.0, 2.0], not a guess.
    asr = [_w(0.0, 0.4, " dit"), _w(0.4, 0.8, " is"), _w(1.0, 2.0, " alex")]
    out = align_text_to_audio("dit is alice", asr)
    assert [w.text for w in out] == [" dit", " is", " alice"]
    assert _spans(out) == pytest.approx([0.0, 0.4, 0.4, 0.8, 1.0, 2.0])


def test_trailing_unmatched_words_stay_after_the_last_match() -> None:
    # Words the human appended past the last ASR word (no audio for them) still get a
    # valid slice after it — exercises the trailing-run branch (j reaches the end).
    asr = [_w(1.0, 2.0, " dit")]
    out = align_text_to_audio("dit foo bar", asr)
    assert [w.text for w in out] == [" dit", " foo", " bar"]
    assert all(w.end > w.start for w in out)  # none collapse to zero
    starts = [w.start for w in out]
    assert starts == sorted(starts)  # non-decreasing; trailing words land at the end


def test_unmatched_leading_words_use_the_time_before_the_first_match() -> None:
    # Words with no ASR match that precede the first matched word get the span *before*
    # it (turn start → first match) split evenly, not collapsed onto the match's start.
    asr = [_w(1.0, 2.0, " Thursday")]
    out = align_text_to_audio("hi there Thursday", asr)
    assert [w.text for w in out] == [" hi", " there", " Thursday"]
    # 0.0→1.0 over two words = 0.5 each, then Thursday at its matched [1.0, 2.0]
    assert _spans(out) == pytest.approx([0.0, 0.5, 0.5, 1.0, 1.0, 2.0])


def test_collapsed_leading_words_get_the_minimum_slice() -> None:
    # When leading words have no audio at all (the first match is at time 0), each still
    # gets exactly _MIN_WORD_S, monotonically — never [t, t] (a zero-length turn).
    asr = [_w(0.0, 2.0, " Thursday")]
    out = align_text_to_audio("Painting and Thursday", asr)
    assert [w.text for w in out] == [" Painting", " and", " Thursday"]
    m = wordtimings._MIN_WORD_S
    assert _spans(out) == pytest.approx([0.0, m, m, 2 * m, 2 * m, 2.0])


def test_empty_inputs() -> None:
    assert align_text_to_audio("", [_w(0.0, 1.0, " hi")]) == []
    assert align_text_to_audio("hi", []) == []


def test_backfill_slices_the_turn_span_and_fills_timings(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    flac = tmp_path / "usb-20260613T120000.flac"
    make_flac(flac)
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
    # The turn spans [+0.5s, +2.5s] in the segment → the slice must be [0.5, 2.5].
    # (A fractional start makes the max(0.0, …) clamp observable.)
    tid = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=0.5),
        end=BASE + timedelta(seconds=2.5),
        text="dit is alice",
        asr_model="human",
    )
    before = store.get_transcript(tid)
    assert before is not None and before.word_timings is None  # a typed correction

    # Spy on the slice so we verify it cuts the *turn's* span, not some other window.
    cut: dict[str, float] = {}

    def spy(src: Path, dst: Path, start: float, end: float) -> None:
        cut["start"], cut["end"] = start, end
        slice_clip(src, dst, start, end)  # the real slice (recall.asr)

    monkeypatch.setattr("recall.wordtimings.slice_clip", spy)

    def transcriber(_clip: Path) -> AsrResult:
        words = (_w(0.0, 0.5, " dit"), _w(0.5, 1.0, " is"), _w(1.0, 2.0, " alice"))
        seg = AsrSegment(0.0, 2.0, "dit is alice", -0.2, 0.0, words=words)
        return AsrResult(language="nl", language_confidence=0.9, segments=(seg,))

    filled = backfill_word_timings(store, transcriber, work_dir=tmp_path / "work")
    assert filled == 1
    assert list((tmp_path / "work").glob("*.wav")) == []  # scratch self-cleaned
    assert (cut["start"], cut["end"]) == pytest.approx((0.5, 2.5))  # the turn's span
    seg = store.get_transcript(tid)
    assert seg is not None and seg.word_timings is not None
    assert [w.text for w in seg.word_timings] == [" dit", " is", " alice"]
    # Idempotent: a second pass finds nothing left to fill.
    assert backfill_word_timings(store, transcriber, work_dir=tmp_path / "work") == 0
