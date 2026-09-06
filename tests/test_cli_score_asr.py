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
    """A transcriber answering per-fixture, keyed by the audio file's stem."""

    def build(_model: str, _base: str, *, words: bool) -> object:
        assert words is False  # plain text pass; word timings not needed

        def transcribe(audio: Path) -> AsrResult:
            stem = audio.stem
            return _result(texts[stem], _language_of(stem))

        return transcribe

    return build


def _language_of(stem: str) -> str:
    for fixture in cli._GOLDEN_FIXTURES:
        if Path(fixture.audio).stem == stem:
            return fixture.language
    raise AssertionError(f"no golden fixture named {stem}")


def _references() -> dict[str, str]:
    """The reference text of every fixture whose audio is actually present."""
    return {
        Path(f.audio).stem: (cli._GOLDEN_FIXTURE / f.reference).read_text()
        for f in cli._GOLDEN_FIXTURES
        if (cli._GOLDEN_FIXTURE / f.audio).exists()
    }


def test_score_asr_passes_when_the_transcript_matches(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setattr(cli, "_build_transcriber", _stub(_references()))
    assert cli.main(["score-asr"]) == 0
    assert "WER" in capsys.readouterr().out


def test_score_asr_fails_when_wer_drifts(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    wrong = {stem: "completely unrelated words entirely" for stem in _references()}
    monkeypatch.setattr(cli, "_build_transcriber", _stub(wrong))
    assert cli.main(["score-asr"]) == 1
    assert "FAIL" in capsys.readouterr().out


def test_every_fixture_marked_committed_really_ships() -> None:
    """`committed=True` has to mean the file is there, or the flag is decoration.

    Deliberately NOT a claim that every household language is covered: `nl` lives
    only in the local-only pair today (#1433), and a test named for coverage it
    does not check is the same defect as a gate advertising a fixture it does not
    have.
    """
    assert any(f.committed for f in cli._GOLDEN_FIXTURES), (
        "no fixture is committed — the gate cannot run on a clone"
    )
    for fixture in cli._GOLDEN_FIXTURES:
        if fixture.committed:
            assert (cli._GOLDEN_FIXTURE / fixture.audio).exists(), fixture.audio
            assert (cli._GOLDEN_FIXTURE / fixture.reference).exists(), fixture.reference


def test_a_missing_committed_fixture_fails_rather_than_passing_vacuously(
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
    tmp_path: Path,
) -> None:
    """A gate with nothing to score must not report success.

    The whole defect behind #1433 was a check that read as repo-wide while its
    audio existed on one machine. Scoring "every fixture that happens to be
    present" reproduces exactly that if the committed one goes missing too.
    """
    monkeypatch.setattr(cli, "_GOLDEN_FIXTURE", tmp_path)
    monkeypatch.setattr(cli, "_build_transcriber", _stub({}))
    assert cli.main(["score-asr"]) == 1
    out = capsys.readouterr().out
    assert "missing" in out.lower()


def test_absent_local_only_fixtures_are_named_not_silently_skipped(
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
    tmp_path: Path,
) -> None:
    """On a fresh clone the private fixtures are absent; the run says so.

    A skip nobody prints is how the gate came to advertise coverage it did not
    have.
    """
    committed = [f for f in cli._GOLDEN_FIXTURES if f.committed]
    optional = [f for f in cli._GOLDEN_FIXTURES if not f.committed]
    assert optional, "nothing to skip — this test no longer measures anything"
    for fixture in committed:
        (tmp_path / fixture.audio).write_bytes(b"")
        (tmp_path / fixture.reference).write_text(
            (cli._GOLDEN_FIXTURE / fixture.reference).read_text()
        )
    references = {
        Path(f.audio).stem: (tmp_path / f.reference).read_text() for f in committed
    }
    monkeypatch.setattr(cli, "_GOLDEN_FIXTURE", tmp_path)
    monkeypatch.setattr(cli, "_build_transcriber", _stub(references))
    assert cli.main(["score-asr"]) == 0
    out = capsys.readouterr().out
    for fixture in optional:
        assert fixture.audio in out
