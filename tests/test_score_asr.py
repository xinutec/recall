"""`python -m recall.score_asr`, with the model stubbed."""

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
    """A fixture whose audio lived on one Mac went unnoticed for months
    (#1433)."""
    assert score_asr.GOLDEN_FIXTURES, "no fixtures — the gate cannot run at all"
    for fixture in score_asr.GOLDEN_FIXTURES:
        assert (score_asr.FIXTURES / fixture.audio).exists(), fixture.audio
        assert (score_asr.FIXTURES / fixture.reference).exists(), fixture.reference


def test_both_household_languages_are_scored_on_any_clone() -> None:
    """The dialogues were committed for their Dutch, which before lived only in
    a fixture on one Mac."""
    languages = {f.language for f in score_asr.GOLDEN_FIXTURES}
    assert {"en", "nl"} <= languages, (
        f"the household speaks en and nl; the gate scores {sorted(languages)}"
    )


def test_a_missing_fixture_fails_rather_than_passing_vacuously(
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
    tmp_path: Path,
) -> None:
    """Scoring only the fixtures present would repeat #1433."""
    monkeypatch.setattr(score_asr, "FIXTURES", tmp_path)
    monkeypatch.setattr(score_asr, "mlx_transcribe", _stub({}))
    assert score_asr.main([]) == 1
    out = capsys.readouterr().out
    assert "missing" in out.lower()
