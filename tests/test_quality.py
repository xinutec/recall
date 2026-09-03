"""Repetition-loop detection (a Whisper artifact guard)."""

from __future__ import annotations

import pytest

from recall.quality import foreign_script_ratio, is_repetition_loop

LOOPS = [
    "I goog goog goog goog goog goog goog goog goog goog",
    "momentum momentum momentum momentum momentum momentum",
    "ASTASTASTASTASTASTASTASTAST",
    "Theobaobaobaobaobaobaobaoba",
    "See you on the phone. See you on the phone. See you on the phone.",
    # short hallucinated loops of a long word (the floor used to miss these)
    "everything everything everything",
    "En dat sembla sembla sembla",
]

REAL = [
    "Iets van de verzameling?",
    "Of Netflix, je weet.",
    "Michiel gelooft ik in een erg goeie film.",
    "Ja, oké.",
    "We need more coffee please before the meeting starts.",
    # short words repeated are real emphasis, not loops
    "No, no, no.",
    "Who? Who? Who?",
    "Boss Boss Boss",
]


@pytest.mark.parametrize("text", LOOPS)
def test_detects_loops(text: str) -> None:
    assert is_repetition_loop(text) is True


@pytest.mark.parametrize("text", REAL)
def test_passes_real_speech(text: str) -> None:
    assert is_repetition_loop(text) is False


def test_foreign_script_ratio() -> None:
    assert foreign_script_ratio("おやすみなさい") == 1.0
    assert foreign_script_ratio("En dan lees je het morgen") == 0.0
    assert foreign_script_ratio("café crème, oké hè") == 0.0  # Latin accents
