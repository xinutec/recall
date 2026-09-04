"""Phase-0 fusion bake-off: WER of the fused window against the best single mic.

For every human correction inside the window, slice the SAME time span from
(a) the reference mic's archived segment and (b) the fused rendering
(audiod's fuse-window output), transcribe both with the bare model — no
vocabulary prompt, exactly like ab-compare/score-asr, because this measures
the model input, not the biasing — and score each against the corrected text.
Symmetric by construction: both arms get identical spans, the same model, the
same decoding.

Non-destructive: reads the store read-only, writes only the report file.

Usage (the ML venv, mlx required):
  .venv/bin/python scripts/fusion_bakeoff.py \
      --db /Volumes/Backup/recall/recall.sqlite \
      --fused /tmp/fusion-bakeoff/fused.wav \
      --start 2026-06-23T20:12:00Z --minutes 30 \
      --reference usb --out /tmp/fusion-bakeoff/report.json [--limit 1]
"""

from __future__ import annotations

import argparse
import json
import sqlite3
import statistics
import subprocess
import tempfile
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path


@dataclass(frozen=True)
class Case:
    """One correction and where its audio lives in both arms."""

    correction_id: int
    truth: str
    start: datetime
    end: datetime
    reference_path: Path
    reference_offset_s: float
    fused_offset_s: float


def _utc(text: str) -> datetime:
    parsed = datetime.fromisoformat(text)
    return parsed if parsed.tzinfo else parsed.replace(tzinfo=UTC)


def load_cases(
    db_path: Path, fused_start: datetime, minutes: int, reference: str
) -> list[Case]:
    """Corrections inside the window whose span one reference segment covers."""
    db = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        rows = db.execute(
            """SELECT c.id, c.corrected_text, ts.start_utc, ts.end_utc
               FROM corrections c
               JOIN transcript_segments ts ON ts.id = c.transcript_segment_id
               ORDER BY ts.start_utc""",
        ).fetchall()
        cases: list[Case] = []
        window_end_s = minutes * 60
        for cid, truth, s_text, e_text in rows:
            start, end = _utc(s_text), _utc(e_text)
            into = (start - fused_start).total_seconds()
            if into < 0 or (end - fused_start).total_seconds() > window_end_s:
                continue
            seg = db.execute(
                """SELECT path, start_utc FROM audio_segments a
                   WHERE a.source_id = ? AND a.start_utc <= ? AND a.end_utc >= ?
                   ORDER BY a.start_utc LIMIT 1""",
                (reference, s_text, e_text),
            ).fetchone()
            if seg is None:
                continue  # span crosses a segment boundary on the reference mic
            cases.append(
                Case(
                    correction_id=int(cid),
                    truth=str(truth),
                    start=start,
                    end=end,
                    reference_path=Path(str(seg[0])),
                    reference_offset_s=(start - _utc(str(seg[1]))).total_seconds(),
                    fused_offset_s=into,
                )
            )
        return cases
    finally:
        db.close()


def slice_wav(src: Path, out: Path, offset_s: float, duration_s: float) -> None:
    """One span as 16 kHz mono WAV — the model-input format for both arms."""
    subprocess.run(
        [
            "ffmpeg",
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-ss",
            f"{max(0.0, offset_s):.3f}",
            "-t",
            f"{duration_s:.3f}",
            "-i",
            str(src),
            "-ac",
            "1",
            "-ar",
            "16000",
            str(out),
        ],
        check=True,
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--db", type=Path, required=True)
    parser.add_argument("--fused", type=Path, required=True)
    parser.add_argument("--start", required=True, help="fused window start, RFC3339")
    parser.add_argument("--minutes", type=int, required=True)
    parser.add_argument("--reference", default="usb")
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--limit", type=int, default=None)
    args = parser.parse_args()

    from recall.asr import combine_result, mlx_transcribe  # noqa: PLC0415 - heavy
    from recall.wer import word_error_rate  # noqa: PLC0415 - keep import cost visible

    fused_start = _utc(args.start.replace("Z", "+00:00"))
    cases = load_cases(args.db, fused_start, args.minutes, args.reference)
    if args.limit is not None:
        cases = cases[: args.limit]
    print(
        f"{len(cases)} corrections in the window "
        f"with a covering {args.reference} segment"
    )

    results: list[dict[str, object]] = []
    with tempfile.TemporaryDirectory(prefix="bakeoff-") as tmp:
        for case in cases:
            duration = (case.end - case.start).total_seconds()
            ref_clip = Path(tmp) / f"{case.correction_id}-ref.wav"
            fus_clip = Path(tmp) / f"{case.correction_id}-fused.wav"
            slice_wav(case.reference_path, ref_clip, case.reference_offset_s, duration)
            slice_wav(args.fused, fus_clip, case.fused_offset_s, duration)
            text_ref, _ = combine_result(mlx_transcribe(ref_clip))
            text_fus, _ = combine_result(mlx_transcribe(fus_clip))
            wer_ref = word_error_rate(case.truth, text_ref)
            wer_fus = word_error_rate(case.truth, text_fus)
            results.append(
                {
                    "correction_id": case.correction_id,
                    "truth": case.truth,
                    "reference_text": text_ref,
                    "fused_text": text_fus,
                    "wer_reference": wer_ref,
                    "wer_fused": wer_fus,
                }
            )
            print(
                f"#{case.correction_id}: {args.reference} {wer_ref:.2f}"
                f"  fused {wer_fus:.2f}  ({duration:.1f}s)"
            )

    if results:
        mean_ref = statistics.mean(float(r["wer_reference"]) for r in results)  # type: ignore[arg-type]
        mean_fus = statistics.mean(float(r["wer_fused"]) for r in results)  # type: ignore[arg-type]
        print(f"\nmean WER — {args.reference}: {mean_ref:.3f}   fused: {mean_fus:.3f}")
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps({"cases": results}, indent=1))
    print(f"wrote {args.out}")


if __name__ == "__main__":
    main()
