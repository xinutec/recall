"""`python -m recall.score_asr` — the golden-fixture WER gate over the real ASR
stack. It was a CLI subcommand until 2026-09-18; the CLI is gone (#1342) and this
is one of the two things kept out of it, because a quality change has to be
judged by a number."""

from __future__ import annotations

from pathlib import Path

import pytest

from recall import score_asr
from recall.asr import AsrResult, AsrSegment


def _result(text: str, language: str) -> AsrResult:
    return AsrResult(
        language=language,
        language_confidence=0.9,
        segments=(
            AsrSegment(
                start=0.0, end=1.0, text=text, avg_logprob=-0.1, no_speech_prob=0.0
            ),
        ),
    )


def _stub(texts: dict[str, str]) -> object:
    """Stands in for `mlx_transcribe`, answering per-fixture by the audio stem."""

    def transcribe(audio: Path, *, model: str, words: bool) -> AsrResult:
        assert words is False  # plain text pass; word timings not needed
        assert model, "the gate must name the model it scored"
        return _result(texts[audio.stem], _language_of(audio.stem))

    return transcribe


def _language_of(stem: str) -> str:
    for fixture in score_asr.GOLDEN_FIXTURES:
        if Path(fixture.audio).stem == stem:
            return fixture.language
    raise AssertionError(f"no golden fixture named {stem}")


def _references() -> dict[str, str]:
    """The reference text of every fixture whose audio is actually present."""
    return {
        Path(f.audio).stem: (score_asr.FIXTURES / f.reference).read_text()
        for f in score_asr.GOLDEN_FIXTURES
        if (score_asr.FIXTURES / f.audio).exists()
    }


def test_score_asr_passes_when_the_transcript_matches(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setattr(score_asr, "mlx_transcribe", _stub(_references()))
    assert score_asr.main([]) == 0
    assert "WER" in capsys.readouterr().out


def test_score_asr_fails_when_wer_drifts(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    wrong = {stem: "completely unrelated words entirely" for stem in _references()}
    monkeypatch.setattr(score_asr, "mlx_transcribe", _stub(wrong))
    assert score_asr.main([]) == 1
    assert "FAIL" in capsys.readouterr().out


def test_every_fixture_really_ships() -> None:
    """A listed fixture has to be there, or the table is decoration.

    This is the check that would have caught #1433 on the day it appeared: the
    gate named a fixture whose audio lived on one Mac, so it read as a repo-wide
    guarantee for months while covering less than it said.
    """
    assert score_asr.GOLDEN_FIXTURES, "no fixtures — the gate cannot run at all"
    for fixture in score_asr.GOLDEN_FIXTURES:
        assert (score_asr.FIXTURES / fixture.audio).exists(), fixture.audio
        assert (score_asr.FIXTURES / fixture.reference).exists(), fixture.reference


def test_both_household_languages_are_scored_on_any_clone() -> None:
    """⚠ Dutch coverage is the REASON the dialogue pair was committed (2026-09-09).

    It could not be claimed before: `nl` lived only in a local-only fixture, so
    this assertion would have passed on one Mac and failed on every clone. If a
    future change makes a fixture local again, this fails and says why.
    """
    languages = {f.language for f in score_asr.GOLDEN_FIXTURES}
    assert {"en", "nl"} <= languages, (
        f"the household speaks en and nl; the gate scores {sorted(languages)}"
    )


def test_a_missing_fixture_fails_rather_than_passing_vacuously(
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
    tmp_path: Path,
) -> None:
    """A gate with nothing to score must not report success.

    The whole defect behind #1433 was a check that read as repo-wide while its
    audio existed on one machine. Scoring "every fixture that happens to be
    present" reproduces exactly that the moment one goes missing.
    """
    monkeypatch.setattr(score_asr, "FIXTURES", tmp_path)
    monkeypatch.setattr(score_asr, "mlx_transcribe", _stub({}))
    assert score_asr.main([]) == 1
    out = capsys.readouterr().out
    assert "missing" in out.lower()
