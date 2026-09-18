"""The golden ASR gate: transcribe the committed speech fixtures with the REAL
model and fail if word error rate drifts past each one's threshold.

The regression net under the model and decoder seams — unit tests stub the ASR,
so nothing else here notices a runtime or decoder change that quietly makes
transcription worse. On demand, never part of `verify`: it loads the model.

⚠ **Its own entry point, like `recall.llmhost`.** It used to be a `recall
score-asr` subcommand, which meant running it imported the whole CLI. The CLI is
gone (#1342) and this is one of the two things kept from it, because a quality
change has to be judged by a number rather than by argument.

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

    Each clip is single-language by construction: a mixed-language one trips
    Whisper's one-language-per-segment detection, which is the documented
    code-switching weakness rather than a regression signal. Several clips may
    share a language — two do — and that is coverage, not duplication.

    ⚠ There is no `committed` flag any more. Every clip here ships with the repo
    (2026-09-09), so a missing one is a FAULT rather than a fresh clone's normal
    state — and that is the safer default for whatever is added next: an absent
    fixture fails loudly instead of quietly narrowing what the gate covers, which
    is the whole of #1433.
    """

    audio: str
    reference: str
    language: str
    threshold: float


# ⚠ EVERY fixture here ships with the repo, and #1433 is why that is worth
# stating. The gate advertised a "committed speech fixture" for months while its
# audio existed on ONE Mac, so it could not run on a clone, in CI, or in a nix
# sandbox — a check that read as a repo-wide guarantee and was not one.
#
# The dialogue pair is macOS `say` reading INVENTED lines (plants, a plumber, a
# bakery), rendered by scripts/gen-speech-fixture.sh. It is nobody's voice and
# says nothing about this household, which is what made committing it safe for a
# public repo; the blanket *.flac ignore that had swallowed it was narrowed on
# 2026-09-09. It carries the only Dutch in the gate.
#
# Thresholds are per fixture, and each is set from ITS OWN measured baseline
# rather than copied, because the references differ in exactness and one number
# would silently import the loosest denominator. Measured 2026-09-06 with
# large-v3-turbo, three identical runs each (the decode is deterministic here, so
# headroom is for a runtime/decoder change, not for run-to-run noise):
#   - dialogue-en 0.0123, dialogue-nl 0.0000 — exact references.
#   - public-domain-en 0.0426 — the CANONICAL poem plus the LibriVox preamble,
#     NOT a transcription of this reading, so the reader's own deviations sit in
#     the baseline permanently (see the fixture README).
# Each threshold is baseline + ~0.05, so all three trip on a regression the size
# of the adapter's real-audio one (~0.05 absolute) — equal DETECTION POWER, not
# an equal number. These are DRIFT bounds; none is evidence about absolute
# transcription quality. If a legitimate runtime update trips one, re-baseline it
# deliberately rather than widening it reflexively.
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

    ⚠ A MISSING fixture FAILS the run rather than being skipped: a gate with less
    to score than it claims must not report success (#1433).

    ⚠ It passes NO vocabulary bias, deliberately — this measures the bare model,
    so that a household name added in the UI cannot move the number.
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
