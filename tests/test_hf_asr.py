"""The adapter-dir detection that routes the accuracy passes to HF vs mlx."""

from __future__ import annotations

import math
from pathlib import Path

from recall.hf_asr import _group_words, is_adapter_dir


def test_a_dir_with_adapter_config_is_an_adapter(tmp_path: Path) -> None:
    (tmp_path / "adapter_config.json").write_text("{}")
    assert is_adapter_dir(str(tmp_path)) is True


def test_a_plain_model_id_is_not_an_adapter() -> None:
    assert is_adapter_dir("mlx-community/whisper-large-v3-turbo") is False


def test_a_dir_without_the_config_is_not_an_adapter(tmp_path: Path) -> None:
    (tmp_path / "weights.bin").write_text("x")
    assert is_adapter_dir(str(tmp_path)) is False


def test_a_missing_path_is_not_an_adapter(tmp_path: Path) -> None:
    assert is_adapter_dir(str(tmp_path / "nope")) is False


def test_group_words_splits_on_leading_spaces() -> None:
    # sub-word tokens: a new word begins at a leading space; " question"+"s" = one word
    tokens = [
        (" how", 0.0, math.log(0.9)),
        (" are", 0.4, math.log(0.9)),
        (" question", 0.8, math.log(0.5)),
        ("s", 1.0, math.log(0.5)),
    ]
    words = _group_words(tokens, clip_end=1.6)
    assert [w.text for w in words] == [" how", " are", " questions"]
    # end of a word is the next word's start; the last word's is clip_end
    assert (words[0].start, words[0].end) == (0.0, 0.4)
    assert (words[2].start, words[2].end) == (0.8, 1.6)


def test_group_words_probability_is_exp_mean_logprob() -> None:
    tokens = [(" question", 0.0, math.log(0.5)), ("s", 0.2, math.log(0.5))]
    (word,) = _group_words(tokens, clip_end=0.6)
    assert word.probability == 0.5  # exp(mean(log .5, log .5)) == .5


def test_group_words_clamps_and_handles_empty() -> None:
    assert _group_words([], clip_end=1.0) == ()
    # a positive logprob (shouldn't happen) clamps to 1.0, not >1
    (word,) = _group_words([(" hi", 0.0, 0.5)], clip_end=0.5)
    assert word.probability == 1.0
