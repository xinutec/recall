"""A/B compare two ASR models on past audio, non-destructively.

Runs both models over a chosen stretch of recording and reports, per audio
segment, the text each produced (so you can see *what changed*), plus — wherever
you have human corrections in that stretch — the word error rate of each model
against that ground truth, so "better" is a number, not a guess. Nothing in the
store is touched: it reads audio + corrections and returns a report to write to a
throwaway file.

The model transcription is injected as two `transcribe(Path) -> str` callables, so
this orchestration is testable with stubs and carries no ML imports. The CLI wires
in the real mlx / LoRA-adapter transcribers (see `recall.cli`).
"""

from __future__ import annotations

import json
from collections.abc import Callable, Sequence
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path

from recall.asr import make_working_copy, scratch_wav, slice_clip
from recall.ids import AudioSegmentId
from recall.store import Store
from recall.wer import normalize_text, word_error_rate

# Transcribe one audio file to its full text.
TranscribeFn = Callable[[Path], str]


@dataclass(frozen=True)
class SegmentDiff:
    """What each model transcribed for one whole audio segment."""

    audio_id: int
    start: datetime
    text_a: str
    text_b: str

    @property
    def changed(self) -> bool:
        return normalize_text(self.text_a) != normalize_text(self.text_b)


@dataclass(frozen=True)
class CorrectionScore:
    """Each model's WER on one human-corrected span (the ground truth)."""

    correction_id: int
    truth: str
    text_a: str
    text_b: str
    wer_a: float
    wer_b: float


@dataclass(frozen=True)
class Report:
    model_a: str
    model_b: str
    segment_diffs: list[SegmentDiff]
    correction_scores: list[CorrectionScore]

    @property
    def n_segments(self) -> int:
        return len(self.segment_diffs)

    @property
    def n_changed(self) -> int:
        return sum(1 for d in self.segment_diffs if d.changed)

    @property
    def mean_wer_a(self) -> float | None:
        return _mean([c.wer_a for c in self.correction_scores])

    @property
    def mean_wer_b(self) -> float | None:
        return _mean([c.wer_b for c in self.correction_scores])


def _mean(xs: list[float]) -> float | None:
    return sum(xs) / len(xs) if xs else None


def compare_models(  # noqa: PLR0913 - two transcribers + selection + naming
    store: Store,
    transcribe_a: TranscribeFn,
    transcribe_b: TranscribeFn,
    *,
    audio_ids: Sequence[AudioSegmentId],
    work_dir: Path,
    model_a: str,
    model_b: str,
) -> Report:
    """Transcribe each audio segment with both models (a whole-segment text diff),
    and WER each model against any human corrections overlapping it. Non-destructive
    — reads audio + corrections, writes nothing to the store. Scratch WAVs self-clean
    (see `scratch_wav`)."""
    work_dir.mkdir(parents=True, exist_ok=True)
    diffs: list[SegmentDiff] = []
    scores: list[CorrectionScore] = []
    for audio_id in audio_ids:
        seg = store.audio_segment(audio_id)
        if seg is None or not Path(seg.path).exists():
            continue
        with scratch_wav(work_dir / f"ab-{int(audio_id)}.wav") as working:
            make_working_copy(Path(seg.path), working)
            diffs.append(
                SegmentDiff(
                    audio_id=int(audio_id),
                    start=seg.start,
                    text_a=transcribe_a(working),
                    text_b=transcribe_b(working),
                )
            )
            # WER each model against the verified text of every correction in this
            # segment — transcribe just that span's audio, like the fine-tune pilot.
            for c in store.human_corrections_overlapping(audio_id, seg.start, seg.end):
                rel_start = max(0.0, (c.start - seg.start).total_seconds())
                rel_end = (c.end - seg.start).total_seconds()
                corr_clip = work_dir / f"ab-{int(audio_id)}-c{int(c.id)}.wav"
                with scratch_wav(corr_clip) as cpath:
                    slice_clip(working, cpath, rel_start, rel_end)
                    text_a = transcribe_a(cpath)
                    text_b = transcribe_b(cpath)
                scores.append(
                    CorrectionScore(
                        correction_id=int(c.id),
                        truth=c.corrected_text,
                        text_a=text_a,
                        text_b=text_b,
                        wer_a=word_error_rate(c.corrected_text, text_a),
                        wer_b=word_error_rate(c.corrected_text, text_b),
                    )
                )
    return Report(
        model_a=model_a,
        model_b=model_b,
        segment_diffs=diffs,
        correction_scores=scores,
    )


def to_dict(report: Report) -> dict[str, object]:
    """JSON-serialisable form of the report (timestamps as ISO strings)."""
    return {
        "model_a": report.model_a,
        "model_b": report.model_b,
        "summary": {
            "segments": report.n_segments,
            "segments_changed": report.n_changed,
            "corrections": len(report.correction_scores),
            "mean_wer_a": report.mean_wer_a,
            "mean_wer_b": report.mean_wer_b,
        },
        "segment_diffs": [
            {
                "audio_id": d.audio_id,
                "start": d.start.isoformat(),
                "changed": d.changed,
                "text_a": d.text_a,
                "text_b": d.text_b,
            }
            for d in report.segment_diffs
        ],
        "correction_scores": [
            {
                "correction_id": c.correction_id,
                "truth": c.truth,
                "text_a": c.text_a,
                "text_b": c.text_b,
                "wer_a": c.wer_a,
                "wer_b": c.wer_b,
            }
            for c in report.correction_scores
        ],
    }


def render_json(report: Report) -> str:
    return json.dumps(to_dict(report), indent=2, ensure_ascii=False)


def _pct(x: float | None) -> str:
    return "—" if x is None else f"{x * 100:.1f}%"


# Truncate a correction's ground-truth text in the markdown WER table.
_TRUTH_PREVIEW = 60


def render_markdown(report: Report) -> str:
    """A human-readable report: a verdict, then the per-correction WER, then the
    segments whose text actually differs between the two models."""
    wa, wb = report.mean_wer_a, report.mean_wer_b
    wa_s, wb_s = _pct(wa), _pct(wb)
    lines = [
        "# A/B model comparison",
        "",
        f"- **A (old):** `{report.model_a}`",
        f"- **B (new):** `{report.model_b}`",
        f"- Segments: {report.n_segments} ({report.n_changed} differ)",
        f"- Corrections scored: {len(report.correction_scores)}",
        f"- Mean WER — A: **{wa_s}**, B: **{wb_s}**",
    ]
    if wa is not None and wb is not None:
        if wb < wa:
            lines.append(f"- **Verdict: B is better** ({wa_s} → {wb_s}).")
        elif wb > wa:
            lines.append(f"- **Verdict: A is better** — B regressed ({wa_s} → {wb_s}).")
        else:
            lines.append("- **Verdict: tie** on WER.")
    else:
        lines.append("- No corrections here — WER unknown; compare the text below.")

    if report.correction_scores:
        lines += ["", "## WER per corrected span", ""]
        lines += ["| id | A | B | ground truth |", "|---|---|---|---|"]
        for c in report.correction_scores:
            truth = c.truth.replace("|", "\\|")
            if len(truth) > _TRUTH_PREVIEW:
                truth = truth[: _TRUTH_PREVIEW - 1] + "…"
            lines.append(
                f"| {c.correction_id} | {_pct(c.wer_a)} | {_pct(c.wer_b)} | {truth} |"
            )

    changed = [d for d in report.segment_diffs if d.changed]
    if changed:
        lines += ["", "## Segments where the models disagree", ""]
        for d in changed:
            lines += [
                f"### segment {d.audio_id} — {d.start.isoformat()}",
                f"- **A:** {d.text_a or '(empty)'}",
                f"- **B:** {d.text_b or '(empty)'}",
                "",
            ]
    return "\n".join(lines).rstrip() + "\n"
