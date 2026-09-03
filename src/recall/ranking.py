"""How the labelling queue ranks turns by *training value*.

A turn is worth your time to label when it is clear (so the label is accurate),
substantial (more speech taught per label), and novel (not a phrase already in
the corpus). This pure scoring is what the /api/train queue sorts by; it has no
heavy deps so it is cheap to compute per request and trivial to test.
"""

from __future__ import annotations

# Loudness at/above which a clip counts as fully clear (≈ close, in-room speech).
CLARITY_REF = 0.05
# Seconds of speech beyond which extra length adds no further training value.
SUBSTANCE_CAP_S = 5.0
# Below this a turn is too short to be worth training on (a one-word fragment).
MIN_DURATION_S = 0.6
# Multiplier for a turn whose text is already in the corpus (re-teaching it is
# low value); it sinks below novel turns but isn't dropped.
REPEAT_PENALTY = 0.3
# Below this many words a turn is too short to judge for word variety.
_MIN_WORDS_FOR_DIVERSITY = 3
# A turn whose letters are mostly non-Latin (CJK, etc.) in a Latin-script
# household is a hallucination — unless the audio is at least this loud, in which
# case it's real speech the model mis-heard and is worth correcting.
FOREIGN_SCRIPT_RATIO = 0.5
FOREIGN_QUIET_FLOOR = 0.03


def training_value(
    *,
    loudness: float,
    duration_s: float,
    repeat: bool,
    diversity: float = 1.0,
    foreign: float = 0.0,
) -> float:
    """Label-worthiness in [0, 1], or negative when not worth labelling at all.

    Clear + substantial + novel + varied ranks highest. Clarity dominates (a
    clean label matters most), length adds signal, an already-labelled phrase is
    discounted, and `diversity` (0..1) sinks repetition-loop garbles. A non-Latin
    transcription (`foreign`, 0..1) on quiet audio is dropped as a hallucination,
    but a loud one is kept — that's real speech the model mis-heard, worth fixing.
    """
    if duration_s < MIN_DURATION_S:
        return -1.0
    if foreign >= FOREIGN_SCRIPT_RATIO and loudness < FOREIGN_QUIET_FLOOR:
        return -1.0
    clarity = min(loudness / CLARITY_REF, 1.0) if CLARITY_REF > 0 else 0.0
    substance = min(duration_s, SUBSTANCE_CAP_S) / SUBSTANCE_CAP_S
    base = 0.6 * clarity + 0.4 * substance
    return base * (REPEAT_PENALTY if repeat else 1.0) * diversity


def normalize_text(text: str) -> str:
    """Casefold + collapse whitespace, for matching a turn against the corpus."""
    return " ".join(text.lower().split())


def diversity_factor(text: str) -> float:
    """0..1 from the fraction of distinct words, squared so repetition is hit hard.

    A repetition-loop hallucination ("...want het jetsetjes" x3) reuses a few
    words, so its ratio is low and it sinks. Single-/two-word turns aren't judged
    (too short to tell) and keep full weight; length/duration handles those.
    """
    words = normalize_text(text).split()
    if len(words) < _MIN_WORDS_FOR_DIVERSITY:
        return 1.0
    ratio = len(set(words)) / len(words)
    return ratio * ratio
