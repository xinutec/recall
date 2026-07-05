"""The bounded pre-detection level normalisation: quiet but real speech gets lifted
enough to clear the gate, while near-silence is capped so it can't be amplified into a
false speech trigger, and already-loud audio is left alone."""

from __future__ import annotations

from recall.vad import _DETECT_MAX_GAIN, _DETECT_TARGET_PEAK, _detection_gain


def test_quiet_speech_is_lifted_toward_the_target() -> None:
    # A clip peaking at ~-47 dB (an un-gained phone catching clear speech) is boosted
    # the full bounded amount, far above where the detector gated it.
    gain = _detection_gain(0.0044)
    assert gain == _DETECT_MAX_GAIN
    assert 0.0044 * gain > 0.1  # comfortably audible to the detector now


def test_near_silence_is_capped_not_amplified_to_speech() -> None:
    # Room tone at ~-66 dB gets only the capped gain, so it stays low — never lifted
    # into the level range the detector would read as speech.
    boosted_peak = 0.0005 * _detection_gain(0.0005)
    assert _detection_gain(0.0005) == _DETECT_MAX_GAIN
    assert boosted_peak < _DETECT_TARGET_PEAK / 4  # still clearly sub-speech


def test_already_loud_audio_is_left_alone() -> None:
    # The USB mic is already at a good level; don't touch it (and never attenuate).
    assert _detection_gain(0.8) == 1.0
    assert _detection_gain(_DETECT_TARGET_PEAK) == 1.0


def test_silence_returns_unit_gain() -> None:
    assert _detection_gain(0.0) == 1.0


def test_moderately_quiet_uses_exact_ratio_below_the_cap() -> None:
    # Just below target and within the cap: scale exactly to the target peak.
    assert _detection_gain(0.1) == _DETECT_TARGET_PEAK / 0.1
