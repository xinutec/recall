"""The golden ASR gate: transcribe the committed speech fixtures with the real
model and fail if word error rate drifts past each one's threshold.

The regression net under the model and decoder seams — unit tests stub the ASR.
On demand, never part of `verify`: it loads the model.

    python -m recall.score_asr [--model MODEL]
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from pathlib import Path

from recall.asr import DEFAULT_MODEL, AsrResult, mlx_transcribe
from recall.wer import word_error_rate

FIXTURES = Path(__file__).resolve().parents[2] / "tests" / "fixtures" / "speech"


@dataclass(frozen=True)
class GoldenFixture:
    """One clip in the golden ASR gate.

    Single-language by construction: a mixed clip trips Whisper's
    one-language-per-segment detection, which is a real code-switching weakness
    rather than a regression signal.
    """

    audio: str
    reference: str
    language: str
    threshold: float


# The dialogue pair is macOS `say` reading invented lines — nobody's voice, which
# is what made committing it to a public repo safe. It carries the gate's only Dutch.
#
# Each threshold is its own measured baseline + ~0.05, never copied: the
# references differ in exactness, and one number would import the loosest
# denominator. These are DRIFT bounds, not evidence about absolute quality. If a
# legitimate runtime update trips one, re-baseline deliberately.
GOLDEN_FIXTURES = (
    GoldenFixture(
        audio="public-domain-en.flac",
        reference="public-domain-en.txt",
        language="en",
        threshold=0.09,
    ),
    GoldenFixture(
        audio="dialogue-en.flac",
        reference="reference-en.txt",
        language="en",
        threshold=0.06,
    ),
    GoldenFixture(
        audio="dialogue-nl.flac",
        reference="reference-nl.txt",
        language="nl",
        threshold=0.05,
    ),
)


def result_text(result: AsrResult) -> str:
    """Full text of a transcription (segments joined)."""
    parts = [s.text.strip() for s in result.segments if s.text.strip()]
    return " ".join(parts).strip()


def main(argv: list[str] | None = None) -> int:
    """Score every fixture and report. Returns 1 if any drifted.

    A missing fixture FAILS rather than being skipped — a gate with less to score
    than it claims must not report success (#1433). Passes no vocabulary bias, so
    a name added in the UI cannot move the number.
    """
    parser = argparse.ArgumentParser(
        prog="recall-score-asr",
        description="Transcribe the committed speech fixtures with the real ASR "
        "stack and fail if WER drifts past each fixture's threshold.",
    )
    parser.add_argument("--model", default=DEFAULT_MODEL, help="mlx-whisper model")
    args = parser.parse_args(argv)

    failed = False
    scored = 0
    for fixture in GOLDEN_FIXTURES:
        audio = FIXTURES / fixture.audio
        if not audio.exists():
            failed = True
            print(f"score-asr: MISSING fixture {fixture.audio}")
            continue
        reference = (FIXTURES / fixture.reference).read_text()
        result = mlx_transcribe(audio, model=args.model, words=False)
        hypothesis = result_text(result)
        wer = word_error_rate(reference, hypothesis)
        detected = result.language
        lang_note = (
            "" if detected == fixture.language else f" (detected language: {detected}!)"
        )
        print(
            f"score-asr[{fixture.audio}]: WER {wer:.3f} vs threshold "
            f"{fixture.threshold}{lang_note}"
        )
        scored += 1
        if wer > fixture.threshold or detected != fixture.language:
            failed = True
            print(f"--- reference ---\n{reference}")
            print(f"--- hypothesis ---\n{hypothesis}")
    if failed:
        print(
            "score-asr: FAIL - transcription drifted; inspect before trusting "
            "the model/decoder change"
        )
        return 1
    print(f"score-asr: ok (model {args.model}, {scored} fixture(s) scored)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
