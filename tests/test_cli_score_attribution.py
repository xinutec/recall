"""`score-attribution` must not compete with a live recorder.

The eval runs the same heavy pyannote + Whisper passes `refine` does, and refine is
idle-gated for a measured reason: two Whispers starve capture (sox buffer overrun =
dropped samples). Nothing enforced that here, so an eval typed at the wrong moment
could cost real speech — the one loss the archive can never recover.
"""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from pathlib import Path

from recall.cli import capture_is_idle, recording_refusal


def _pause(root: Path, minutes: int) -> None:
    (root / "capture_paused_until").write_text(
        (datetime.now(UTC) + timedelta(minutes=minutes)).isoformat()
    )


def test_capture_is_idle_only_while_a_pause_is_live(tmp_path: Path) -> None:
    assert capture_is_idle(tmp_path) is False  # no marker at all = recording
    _pause(tmp_path, 30)
    assert capture_is_idle(tmp_path) is True
    _pause(tmp_path, -1)  # elapsed pause: capture has come back on its own
    assert capture_is_idle(tmp_path) is False


def test_a_live_recorder_refuses_the_run_and_says_how_to_proceed(
    tmp_path: Path,
) -> None:
    refusal = recording_refusal(tmp_path, allow=False)
    assert refusal is not None
    # The message has to carry both halves: why it stopped, and the two ways forward.
    assert "recording" in refusal
    assert "recall pause" in refusal
    assert "--while-recording" in refusal


def test_a_paused_recorder_lets_the_run_start(tmp_path: Path) -> None:
    _pause(tmp_path, 30)
    assert recording_refusal(tmp_path, allow=False) is None


def test_the_override_is_honoured_but_only_when_asked_for(tmp_path: Path) -> None:
    # An explicit flag, never an inferred fallback: scoring an uploaded meeting still
    # burns the same GPU the household mic's transcription needs.
    assert recording_refusal(tmp_path, allow=True) is None
    assert recording_refusal(tmp_path, allow=False) is not None
