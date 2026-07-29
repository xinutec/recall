"""Score how well diarization + word-alignment attributes words to the right speaker,
against human-corrected ground truth.

The split work a person does in the session view *is* ground truth for who-said-what —
but, unlike text corrections (which train the ASR model), it currently feeds nothing
back to the attribution step. This gives that loop a number: replay a corrected
recording's audio through diarize + `assign_words_to_speakers`, and measure the fraction
of words placed with the right speaker. With it, the alignment knobs (`_MIN_TURN_S`, the
assignment heuristic) can be tuned on real corrections instead of guessed.

Pure (predicted word→cluster + truth spans in, a score out), so it's unit-tested; the
heavy diarize/ASR replay is the caller's (the `score-attribution` CLI).
"""

from __future__ import annotations

import itertools
from collections import Counter
from collections.abc import Sequence
from dataclasses import dataclass


@dataclass(frozen=True)
class TruthSpan:
    """A stretch a human confirmed one speaker said — seconds, segment-relative."""

    start: float
    end: float
    speaker: str


@dataclass(frozen=True)
class AttributionScore:
    """How many ground-truth-covered words were placed with the right speaker."""

    words: int
    correct: int

    @property
    def accuracy(self) -> float:
        return self.correct / self.words if self.words else 0.0


def _span_at(t: float, truth: Sequence[TruthSpan]) -> TruthSpan | None:
    for span in truth:
        if span.start <= t < span.end:
            return span
    return None


def _truth_at(t: float, truth: Sequence[TruthSpan]) -> str | None:
    span = _span_at(t, truth)
    return span.speaker if span is not None else None


@dataclass(frozen=True)
class _Verdict:
    """One ground-truth-covered word: where it is, its true and predicted speaker."""

    midpoint: float
    truth: str
    predicted: str
    span_len: float

    @property
    def correct(self) -> bool:
        return self.truth == self.predicted


def _verdicts(
    predicted: Sequence[tuple[float, str]], truth: Sequence[TruthSpan]
) -> list[_Verdict]:
    """Per-word truth-vs-predicted, with clusters mapped to the truth name they most
    fall under (so cluster ids vs names don't matter, and over-/under-clustering is
    penalised fairly). Words outside every truth span are dropped — nothing to score.
    """
    in_truth: list[tuple[float, TruthSpan, str]] = []
    for midpoint, cluster in predicted:
        span = _span_at(midpoint, truth)
        if span is not None:
            in_truth.append((midpoint, span, cluster))
    if not in_truth:
        return []

    by_cluster: dict[str, Counter[str]] = {}
    for _, span, cluster in in_truth:
        by_cluster.setdefault(cluster, Counter())[span.speaker] += 1
    mapping = {c: counts.most_common(1)[0][0] for c, counts in by_cluster.items()}

    return [
        _Verdict(midpoint, span.speaker, mapping[cluster], span.end - span.start)
        for midpoint, span, cluster in in_truth
    ]


def score_attribution(
    predicted: Sequence[tuple[float, str]], truth: Sequence[TruthSpan]
) -> AttributionScore:
    """Fraction of ground-truth-covered words placed with the right speaker — see
    `_verdicts` for the cluster→name mapping."""
    verdicts = _verdicts(predicted, truth)
    return AttributionScore(
        words=len(verdicts), correct=sum(v.correct for v in verdicts)
    )


def _change_points(truth: Sequence[TruthSpan]) -> list[float]:
    """Times where the speaker changes between consecutive (in time) truth spans — the
    spots where attribution is most likely to cross."""
    spans = sorted(truth, key=lambda s: s.start)
    return [a.end for a, b in itertools.pairwise(spans) if a.speaker != b.speaker]


@dataclass(frozen=True)
class AttributionReport:
    """Where the misattributed words are, so the right lever is provable."""

    words: int
    correct: int
    near_words: int  # words within `boundary_window` of a speaker change
    near_correct: int
    short_words: int  # words inside a truth span shorter than `short_span`
    short_correct: int
    errors_by_speaker: dict[str, int]  # truth name → how many of its words were stolen

    @staticmethod
    def empty() -> AttributionReport:
        """The identity for `merged_with` — what an eval starts accumulating from."""
        return AttributionReport(0, 0, 0, 0, 0, 0, {})

    def merged_with(self, other: AttributionReport) -> AttributionReport:
        """This report plus `other`. The eval scores one segment at a time and sums
        them, so the reported accuracy is over every word of the recording rather than a
        mean of per-segment rates (which would weight a 3-word segment like a 300-word
        one)."""
        errors = Counter(self.errors_by_speaker)
        errors.update(other.errors_by_speaker)
        return AttributionReport(
            words=self.words + other.words,
            correct=self.correct + other.correct,
            near_words=self.near_words + other.near_words,
            near_correct=self.near_correct + other.near_correct,
            short_words=self.short_words + other.short_words,
            short_correct=self.short_correct + other.short_correct,
            errors_by_speaker=dict(errors),
        )

    @staticmethod
    def _pct(correct: int, total: int) -> float:
        return correct / total if total else 0.0

    @property
    def accuracy(self) -> float:
        return self._pct(self.correct, self.words)

    @property
    def near_accuracy(self) -> float:
        return self._pct(self.near_correct, self.near_words)

    @property
    def interior_accuracy(self) -> float:
        return self._pct(self.correct - self.near_correct, self.words - self.near_words)

    @property
    def short_accuracy(self) -> float:
        return self._pct(self.short_correct, self.short_words)


def attribution_report(
    predicted: Sequence[tuple[float, str]],
    truth: Sequence[TruthSpan],
    *,
    boundary_window: float = 1.0,
    short_span: float = 2.0,
) -> AttributionReport:
    """Localise the misattributed words: split accuracy into words *near a speaker
    change* (within `boundary_window`s) vs *interior*, words inside *short* turns, and a
    per-speaker tally of whose words were taken — so we can tell a boundary/alignment
    problem from a segmentation/voice one."""
    verdicts = _verdicts(predicted, truth)
    changes = _change_points(truth)
    near_words = near_correct = short_words = short_correct = correct = 0
    errors: Counter[str] = Counter()
    for v in verdicts:
        correct += v.correct
        if not v.correct:
            errors[v.truth] += 1
        dist = min((abs(v.midpoint - c) for c in changes), default=float("inf"))
        if dist <= boundary_window:
            near_words += 1
            near_correct += v.correct
        if v.span_len < short_span:
            short_words += 1
            short_correct += v.correct
    return AttributionReport(
        words=len(verdicts),
        correct=correct,
        near_words=near_words,
        near_correct=near_correct,
        short_words=short_words,
        short_correct=short_correct,
        errors_by_speaker=dict(errors),
    )
