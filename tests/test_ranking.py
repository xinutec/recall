"""Training-value ranking of labelling candidates."""

from __future__ import annotations

from pytest import approx

from recall.quality import foreign_script_ratio
from recall.ranking import (
    diversity_factor,
    normalize_text,
    training_value,
)


def test_clear_long_novel_outranks_quiet_short_repeat() -> None:
    good = training_value(loudness=0.1, duration_s=4.0, repeat=False)
    filler = training_value(loudness=0.005, duration_s=0.8, repeat=True)
    assert good > filler


def test_too_short_is_negative() -> None:
    assert training_value(loudness=0.5, duration_s=0.3, repeat=False) < 0.0


def test_repeat_is_penalised() -> None:
    novel = training_value(loudness=0.1, duration_s=3.0, repeat=False)
    repeat = training_value(loudness=0.1, duration_s=3.0, repeat=True)
    assert 0 < repeat < novel


def test_longer_turn_scores_higher_all_else_equal() -> None:
    short = training_value(loudness=0.1, duration_s=1.0, repeat=False)
    long = training_value(loudness=0.1, duration_s=4.0, repeat=False)
    assert long > short


def test_normalize_text_casefolds_and_collapses_whitespace() -> None:
    assert normalize_text("  Okay,   That is Fine.  ") == "okay, that is fine."


def test_repetition_loop_has_low_diversity() -> None:
    garble = "maar deze want het jetsetjes want het jetsetjes want het jetsetjes"
    real = "en dan lees je het morgen en overmorgen ik ga het opschrijven"
    assert diversity_factor(garble) < diversity_factor(real)


def test_short_turns_keep_full_diversity() -> None:
    assert diversity_factor("okay") == 1.0
    assert diversity_factor("ja gewoon") == 1.0


def test_diversity_sinks_a_loud_long_loop_below_a_real_turn() -> None:
    # Both loud and long; only diversity separates the loop garble from real speech.
    loop = training_value(loudness=0.2, duration_s=5.0, repeat=False, diversity=0.18)
    real = training_value(loudness=0.1, duration_s=3.0, repeat=False, diversity=0.7)
    assert loop < real


def test_quiet_foreign_script_is_dropped_but_loud_is_kept() -> None:
    # A non-Latin hallucination on silence is junk...
    quiet = training_value(loudness=0.01, duration_s=1.0, repeat=False, foreign=1.0)
    assert quiet < 0
    # ...but a loud one is real speech mis-heard — keep it for correcting.
    loud = training_value(loudness=0.1, duration_s=1.0, repeat=False, foreign=1.0)
    assert loud > 0


# --- exact-formula characterisation (pins the scoring, not just its ordering) ---
# clarity = min(loudness / 0.05, 1);  substance = min(duration, 5) / 5
# base    = 0.6 * clarity + 0.4 * substance;  value = base * (0.3 if repeat) * diversity


def test_training_value_exact_formula() -> None:
    # clarity = 0.025/0.05 = 0.5, substance = 2.5/5 = 0.5, base = 0.3 + 0.2 = 0.5.
    assert training_value(loudness=0.025, duration_s=2.5, repeat=False) == approx(0.5)


def test_clarity_caps_at_one() -> None:
    # loudness/0.05 = 2.0 but clarity is capped at 1.0; substance = 1.0 -> base 1.0.
    assert training_value(loudness=0.1, duration_s=5.0, repeat=False) == approx(1.0)


def test_repeat_penalty_is_exactly_0_3() -> None:
    # base 0.5 * REPEAT_PENALTY 0.3 = 0.15.
    assert training_value(loudness=0.025, duration_s=2.5, repeat=True) == approx(0.15)


def test_diversity_scales_the_value_linearly() -> None:
    # base 0.5 * diversity 0.5 = 0.25.
    base = training_value(loudness=0.025, duration_s=2.5, repeat=False, diversity=0.5)
    assert base == approx(0.25)


def test_duration_exactly_at_the_floor_is_kept() -> None:
    # 0.6 is the floor; `< MIN_DURATION_S` must not drop a turn that sits on it.
    assert training_value(loudness=0.5, duration_s=0.6, repeat=False) > 0


def test_foreign_exactly_at_ratio_floor_and_quiet_is_dropped() -> None:
    # foreign == 0.5 with quiet audio must drop (the test is `>=`, not `>`).
    assert training_value(loudness=0.01, duration_s=1.0, repeat=False, foreign=0.5) < 0


def test_foreign_at_exactly_the_quiet_floor_is_kept() -> None:
    # loudness 0.03 is NOT below the floor (`<`), so a loud-enough foreign turn stays.
    value = training_value(loudness=0.03, duration_s=1.0, repeat=False, foreign=1.0)
    assert value > 0


def test_diversity_factor_three_word_loop_is_squared_ratio() -> None:
    # 3 words (the judge-it threshold), 1 distinct: ratio 1/3, squared = 1/9.
    assert diversity_factor("ja ja ja") == approx(1 / 9)


def test_foreign_script_ratio_mixed_is_the_fraction() -> None:
    # one Latin + one Hiragana letter -> 1/2 non-Latin.
    assert foreign_script_ratio("aあ") == approx(0.5)


def test_foreign_script_ratio_no_letters_is_zero() -> None:
    # Digits/punctuation only: no letters, so nothing foreign (and no divide-by-zero).
    assert foreign_script_ratio("123 ... !?") == 0.0
