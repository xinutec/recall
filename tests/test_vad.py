"""The pre-detection level normalisation: a clip's peak is lifted to the target, however
quiet it is, and already-loud audio is left alone. There is no cap — the one there was
silenced the quietest phone in the house (#1485)."""

from __future__ import annotations

from recall.vad import _DETECT_TARGET_PEAK, _detection_gain


def test_quiet_speech_is_lifted_to_the_target() -> None:
    # A phone across the room peaks at -62 dBFS. It is lifted all the way.
    assert _detection_gain(0.00079) == _DETECT_TARGET_PEAK / 0.00079


def test_already_loud_audio_is_left_alone() -> None:
    # The USB mic is already at a good level; don't touch it (and never attenuate).
    assert _detection_gain(0.8) == 1.0
    assert _detection_gain(_DETECT_TARGET_PEAK) == 1.0


def test_silence_returns_unit_gain() -> None:
    assert _detection_gain(0.0) == 1.0


def test_moderately_quiet_uses_the_exact_ratio() -> None:
    assert _detection_gain(0.1) == _DETECT_TARGET_PEAK / 0.1
