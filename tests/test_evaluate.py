"""WER scoring of transcriptions against ground-truth corrections (pure)."""

from __future__ import annotations

import pytest

from recall.evaluate import score_clips


def test_score_clips_aggregates_word_wer() -> None:
    records = [
        {"text": "hello world", "language": "en"},
        {"text": "foo bar baz", "language": "nl"},
    ]
    hyps = ["hello world", "foo qux baz"]  # 0 errors, then 1 of 3 wrong

    report = score_clips(records, hyps)

    # 5 reference words, 1 wrong -> 20%.
    assert report.clips == 2
    assert report.ref_words == 5
    assert report.wer == pytest.approx(0.2)
    # Per-clip detail is preserved in order, with the original (un-normalised) text.
    assert report.per_clip[0].wer == pytest.approx(0.0)
    assert report.per_clip[1].wer == pytest.approx(1 / 3)
    assert report.per_clip[1].language == "nl"
    assert report.per_clip[1].hyp == "foo qux baz"


def test_score_clips_ignores_punctuation_and_case() -> None:
    report = score_clips(
        [{"text": "Carol, is dit dringend?"}], ["carol is dit dringend"]
    )
    assert report.wer == pytest.approx(0.0)


def test_score_clips_empty_reference_does_not_divide_by_zero() -> None:
    report = score_clips([{"text": ""}], ["something"])
    assert report.ref_words == 0
    assert report.wer == 0.0


def test_score_clips_rejects_length_mismatch() -> None:
    with pytest.raises(ValueError, match="length"):
        score_clips([{"text": "a"}], ["a", "b"])
