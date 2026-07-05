"""Detect Whisper repetition-loop output ("goog goog goog…", "ASTASTAST…").

A source-level output guard: even with the decode-time guards, Whisper still
occasionally loops on hard/short audio (especially the live path). No human
utterance repeats a token dozens of times, so this is safe to drop — applied when
writing turns (live + worker) and as a one-off cleanup of existing loops.
"""

from __future__ import annotations

import re
from collections import Counter

_WORD_MIN = 6  # need a few words before a dominant one means "loop"
_WORD_FRACTION = 0.5  # one token is >= half the words
_MAX_PHRASE_WORDS = 6  # a repeated "phrase" up to this many words
_MIN_PHRASE_REPEATS = 3  # repeated at least this many times
# A 2-8 char unit repeated 4+ times in a row (e.g. "ASTASTASTAST", "obaobaoba").
_CHAR_LOOP_RE = re.compile(r"(.{2,8}?)\1{3,}")
_CHAR_LOOP_MIN_LEN = 12
# A word repeated 3+ times in a row is a loop *only* if it's long enough — short
# words are real emphasis ("no no no", "who who who"), long ones are hallucinations
# ("everything everything everything"). 6 chars is the split, calibrated from the
# archive (repeated lengths 2-3 are real; 6+ — sembla/momentum/everything — loops).
_RUN_MIN = 3
_RUN_WORD_MIN_LEN = 6


def _longest_consecutive_run(words: list[str]) -> tuple[int, str]:
    """The longest run of one word repeated back-to-back, and that word."""
    best, best_word, run, prev = 0, "", 0, ""
    for word in words:
        run = run + 1 if word == prev else 1
        if run > best:
            best, best_word = run, word
        prev = word
    return best, best_word


def _is_word_loop(text: str) -> bool:
    words = re.findall(r"\w+", text.lower())
    if not words:
        return False
    # a long word repeated back-to-back — catches short hallucinated loops the
    # word-count floor below would miss ("everything everything everything")
    run, run_word = _longest_consecutive_run(words)
    if run >= _RUN_MIN and len(run_word) >= _RUN_WORD_MIN_LEN:
        return True
    if len(words) < _WORD_MIN:
        return False
    # one token dominates (e.g. "momentum momentum momentum…")
    if Counter(words).most_common(1)[0][1] / len(words) >= _WORD_FRACTION:
        return True
    # a short phrase repeated back-to-back ("see you on the phone" x3)
    for period in range(1, _MAX_PHRASE_WORDS + 1):
        reps = len(words) // period
        if reps < _MIN_PHRASE_REPEATS:
            continue
        if words[: period * reps] == words[:period] * reps:
            return True
    return False


def _is_char_loop(text: str) -> bool:
    """Space-less loops ("ASTASTAST", "obaobaoba"): a short unit repeated in a row."""
    compact = re.sub(r"\s", "", text.lower())
    match = _CHAR_LOOP_RE.search(compact)
    return match is not None and len(match.group(0)) >= _CHAR_LOOP_MIN_LEN


def is_repetition_loop(text: str) -> bool:
    """True if `text` is a degenerate repetition loop (a model artifact)."""
    return _is_word_loop(text) or _is_char_loop(text)
