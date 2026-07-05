"""Word error rate — the metric for measuring and tuning transcription accuracy.

WER = (substitutions + deletions + insertions) / reference word count, computed
as the word-level Levenshtein distance. Text is normalised (lowercased,
punctuation stripped) before comparison unless disabled.
"""

from __future__ import annotations

import re

_PUNCT_RE = re.compile(r"[^\w\s]", re.UNICODE)


def normalize_text(text: str) -> str:
    """Lowercase, strip punctuation, and collapse whitespace."""
    return " ".join(_PUNCT_RE.sub(" ", text.lower()).split())


def word_error_rate(
    reference: str, hypothesis: str, *, normalize: bool = True
) -> float:
    """Word error rate of `hypothesis` against `reference` (0.0 = perfect)."""
    if normalize:
        reference = normalize_text(reference)
        hypothesis = normalize_text(hypothesis)
    ref = reference.split()
    hyp = hypothesis.split()
    if not ref:
        return 0.0 if not hyp else 1.0

    previous = list(range(len(hyp) + 1))
    for i, ref_word in enumerate(ref, start=1):
        current = [i]
        for j, hyp_word in enumerate(hyp, start=1):
            cost = 0 if ref_word == hyp_word else 1
            current.append(
                min(previous[j] + 1, current[j - 1] + 1, previous[j - 1] + cost)
            )
        previous = current
    return previous[-1] / len(ref)
