"""Word error rate — the accuracy metric for tuning transcription."""

from __future__ import annotations

from recall.wer import normalize_text, word_error_rate


def test_identical_is_zero() -> None:
    assert word_error_rate("the quick brown fox", "the quick brown fox") == 0.0


def test_one_substitution() -> None:
    # 1 wrong word out of 4
    assert word_error_rate("the quick brown fox", "the quick green fox") == 0.25


def test_one_deletion() -> None:
    assert word_error_rate("the quick brown fox", "the quick fox") == 0.25


def test_one_insertion() -> None:
    # insertion counts against the reference length
    assert word_error_rate("the quick brown fox", "the quick brown red fox") == 0.25


def test_empty_reference() -> None:
    assert word_error_rate("", "") == 0.0
    assert word_error_rate("", "spurious words") == 1.0


def test_empty_hypothesis_is_total_error_not_more() -> None:
    # Transcriber produced nothing for real speech: every ref word is a deletion,
    # so WER is exactly 1.0 — never (len+1)/len from an off-by-one in the DP base.
    assert word_error_rate("the quick brown fox", "") == 1.0


def test_normalization_ignores_case_and_punctuation() -> None:
    assert word_error_rate("Hello, world!", "hello world") == 0.0


def test_normalize_text() -> None:
    assert normalize_text("  Hello,   WORLD!! ") == "hello world"
