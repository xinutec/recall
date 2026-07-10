"""The adapter-dir detection that routes the accuracy passes to HF vs mlx."""

from __future__ import annotations

import itertools
import math
from pathlib import Path

from recall.asr import AsrResult, AsrSegment, Word
from recall.hf_asr import (
    _group_words,
    is_adapter_dir,
    merge_windows,
    plan_windows,
    transcribe_windowed,
)


def _seg(
    start: float, end: float, text: str, *words: tuple[float, float, str]
) -> AsrSegment:
    return AsrSegment(
        start=start,
        end=end,
        text=text,
        avg_logprob=-0.2,
        no_speech_prob=0.0,
        words=tuple(Word(start=s, end=e, text=t, probability=0.9) for s, e, t in words),
    )


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


def test_plan_windows_covers_the_whole_clip_without_gaps() -> None:
    # 75 s at 16 kHz → three windows (30 s, 30 s, 15 s), contiguous and gap-free.
    windows = plan_windows(75 * 16000, 16000, 30.0)
    assert windows == [(0, 480000), (480000, 960000), (960000, 1200000)]
    assert windows[0][0] == 0
    assert windows[-1][1] == 75 * 16000  # last window reaches the end of the clip
    for (_a, b), (c, _d) in itertools.pairwise(windows):
        assert b == c  # no gap between windows


def test_plan_windows_empty_clip_has_no_windows() -> None:
    assert plan_windows(0, 16000, 30.0) == []


def test_merge_windows_offsets_each_window_to_clip_time() -> None:
    w0 = AsrResult("en", None, (_seg(0.0, 5.0, " hello", (0.0, 5.0, " hello")),))
    w1 = AsrResult("en", None, (_seg(0.0, 4.0, " again", (0.0, 4.0, " again")),))
    merged = merge_windows([(0.0, w0), (30.0, w1)])
    assert [s.text for s in merged.segments] == [" hello", " again"]
    # the second window's segment and its words are shifted by the 30 s offset
    assert (merged.segments[1].start, merged.segments[1].end) == (30.0, 34.0)
    assert merged.segments[1].words[0].start == 30.0


def test_merge_windows_empty_is_a_valid_empty_result() -> None:
    merged = merge_windows([])
    assert merged.segments == ()


def test_transcribe_windowed_transcribes_every_window_not_just_the_first() -> None:
    # The regression guard for the 30 s truncation bug: a 90 s clip must reach the model
    # as three windows and come back spanning the whole clip, not stop at 0:30.
    sr = 16000
    array = list(range(90 * sr))  # a stand-in waveform; only its length matters here
    seen: list[int] = []

    def fake_window(chunk: list[int]) -> AsrResult:
        seen.append(len(chunk))
        return AsrResult("en", None, (_seg(0.0, len(chunk) / sr, "chunk"),))

    result = transcribe_windowed(array, sr, fake_window)
    assert len(seen) == 3  # three windows transcribed, not one
    assert len(result.segments) == 3
    assert result.segments[0].start == 0.0
    assert result.segments[-1].end == 90.0  # coverage reaches the end of the clip
