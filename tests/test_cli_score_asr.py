"""`recall score-asr` — the golden-fixture WER gate over the real ASR stack."""

from __future__ import annotations

from pathlib import Path

import pytest

from recall import cli
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
    """A transcriber that answers per-language from the fixture filename."""

    def build(_model: str, _base: str, *, words: bool) -> object:
        assert words is False  # plain text pass; word timings not needed

        def transcribe(audio: Path) -> AsrResult:
            lang = audio.stem.removeprefix("dialogue-")
            return _result(texts[lang], lang)

        return transcribe

    return build


def test_score_asr_passes_when_the_transcript_matches(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    fixtures = Path(__file__).parent / "fixtures" / "speech"
    references = {
        lang: (fixtures / f"reference-{lang}.txt").read_text() for lang in ("en", "nl")
    }
    monkeypatch.setattr(cli, "_build_transcriber", _stub(references))
    assert cli.main(["score-asr"]) == 0
    assert "WER" in capsys.readouterr().out


def test_score_asr_fails_when_wer_drifts(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    wrong = {
        "en": "completely unrelated words entirely",
        "nl": "helemaal andere woorden",
    }
    monkeypatch.setattr(cli, "_build_transcriber", _stub(wrong))
    assert cli.main(["score-asr"]) == 1
    assert "FAIL" in capsys.readouterr().out
