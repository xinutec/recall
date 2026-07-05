"""Score transcriptions against ground-truth corrections (WER), no model.

Pure aggregation used by the fine-tune pilot to compare a base model and a LoRA
adapter on the same held-out clips. The transcription itself is produced
elsewhere (recall.finetune) and injected as plain strings, so this is testable
without any ML environment.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from recall.wer import normalize_text, word_error_rate


@dataclass(frozen=True)
class ClipScore:
    """One held-out clip's outcome: ground-truth vs hypothesis and its WER."""

    ref: str
    hyp: str
    language: str | None
    wer: float


@dataclass(frozen=True)
class EvalReport:
    """Aggregate word-WER over a set of clips, plus per-clip detail."""

    wer: float
    ref_words: int
    clips: int
    per_clip: tuple[ClipScore, ...]


def score_clips(records: list[dict[str, Any]], hyps: list[str]) -> EvalReport:
    """Word-WER of `hyps` against each record's `text` (the corrected truth).

    Aggregates as a single word-weighted WER (longer turns count more), and
    keeps the per-clip breakdown for inspection. Empty references contribute no
    weight rather than dividing by zero.
    """
    if len(records) != len(hyps):
        raise ValueError(f"records/hyps length mismatch: {len(records)} != {len(hyps)}")

    scores: list[ClipScore] = []
    total_ref = 0
    total_err = 0.0
    for record, hyp in zip(records, hyps, strict=True):
        ref_text = record.get("text", "")
        ref = normalize_text(ref_text)
        wer = word_error_rate(ref, normalize_text(hyp))
        ref_words = len(ref.split())
        total_ref += ref_words
        total_err += wer * ref_words
        scores.append(
            ClipScore(
                ref=ref_text,
                hyp=hyp,
                language=record.get("language"),
                wer=wer,
            )
        )

    aggregate = (total_err / total_ref) if total_ref else 0.0
    return EvalReport(
        wer=aggregate,
        ref_words=total_ref,
        clips=len(records),
        per_clip=tuple(scores),
    )
