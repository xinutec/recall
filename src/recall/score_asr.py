"""The golden ASR check: transcribe the committed speech fixtures with the
real model and fail if a word error rate drifts past its threshold. Run on
demand, since it loads the model; the unit tests stub the ASR.

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
    """One clip. One language each: Whisper detects one language per segment,
    so a mixed clip measures that weakness, not drift."""

    audio: str
    reference: str
    language: str
    threshold: float


# The dialogues are macOS `say` reading invented lines, so nobody's voice is
# in the public repo; one is the only Dutch.
#
# Each threshold is that fixture's measured baseline plus about 0.05: bounds on
# drift, not measures of quality. Re-baseline on purpose if an update trips one.
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
    """Score every fixture; 1 if any drifted or is missing (#1433). No
    vocabulary prompt, so a name added in the UI cannot move the numbers."""
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
