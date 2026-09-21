"""Raw vs calibrated room selection, refereed against human corrections (#1461).

For each correction, asks which microphone each RANK would have chosen for its
block, takes what that microphone actually transcribed, and scores both against
the corrected text. Reports the median.

Three constraints, each of which produced a wrong answer when missed:

  - Compare STORED TRANSCRIPTS, never re-transcribed slices. The correction's
    span comes from one source's timeline and the mics do not share a clock —
    pixel9 ran 3.7 s behind usb — so slicing one arm's span out of the other's
    clip scores the wrong audio. Matching is by overlap within a tolerance.
  - Exclude HUMAN CORRECTIONS from the hypothesis. A correction is stored as a
    turn that supersedes the machine's, so reading the current turn makes the
    hypothesis identical to the truth.
  - Take only the LATEST machine version per moment; the archive keeps every
    re-transcription.

A block where both ranks choose the same microphone is skipped: both arms would
carry identical audio, which is how the June window refereed this twice and
answered nothing.

Every database is opened read-only; only the report is written.

Usage (no model needed — it reads what the pipeline already wrote):
  python3 scripts/rank_referee.py \
      --db <fleet recall.sqlite> --ingest <fleet ingest.sqlite> \
      --start 2026-09-03T10:00:00Z --minutes 60 --out /tmp/referee.json
"""

from __future__ import annotations

import argparse
import json
import sqlite3
import statistics
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
from pathlib import Path

BOTH_ARMS = 2
"""Corrections must be typed over BOTH competing microphones, or the set scores
one arm against its own words and the other against a stranger's."""

DEFAULT_TOLERANCE_S = 6.0
"""Cross-mic skew a turn may sit away and still match. Sized from a measured
3.7 s, and printed in the report so a reader sees what it admitted."""


@dataclass(frozen=True)
class Case:
    correction_id: int
    truth: str
    raw_source: str
    calibrated_source: str
    raw_text: str
    calibrated_text: str
    origin_source: str
    """Which microphone's words the human EDITED.

    ⚠⚠ Without this the report cannot be read at all. A correction is an edit of
    one microphone's own text, so the arm it came from shares that arm's
    vocabulary and phrasing and is flattered by it — and a median taken over
    corrections of mixed, unrecorded origin averages a self-score with a
    cross-score and reports the blend as if it were a comparison.
    """


def _utc(text: str) -> datetime:
    parsed = datetime.fromisoformat(text.replace("Z", "+00:00"))
    return parsed if parsed.tzinfo else parsed.replace(tzinfo=UTC)


def _block_of(when: datetime) -> str:
    """The UTC-aligned minute a time falls in, as the builder stamps it."""
    return when.replace(second=0, microsecond=0).strftime("%Y-%m-%dT%H:%M:%SZ")


def winners(ingest: sqlite3.Connection, block: str) -> tuple[str, str] | None:
    """(raw winner, calibrated winner) for a block, or None if unrankable.

    From the verdict the builder recorded, so this scores the choice production
    would have made at the time.
    """
    row = ingest.execute(
        "SELECT contributors FROM room_blocks WHERE start_utc = ?", (block,)
    ).fetchone()
    if row is None:
        return None
    rankable = [
        c
        for c in json.loads(row[0])
        if c.get("speech_db") is not None and c.get("calibrated") is not None
    ]
    if not rankable:
        return None
    return (
        max(rankable, key=lambda c: c["speech_db"])["source"],
        max(rankable, key=lambda c: c["calibrated"])["source"],
    )


def heard(
    db: sqlite3.Connection,
    source: str,
    start: datetime,
    end: datetime,
    tolerance_s: float,
) -> str | None:
    """What `source` transcribed over the span, within the skew tolerance.

    Every overlapping machine turn joined in time order: one utterance is often
    several turns on one microphone and one on another, so the nearest turn alone
    would score a fragment against a sentence.
    """
    rows = db.execute(
        """SELECT ts.start_utc, ts.text FROM transcript_segments ts
             JOIN audio_segments a ON a.id = ts.audio_segment_id
            WHERE a.source_id = ?
              AND ts.start_utc <= ? AND ts.end_utc >= ?
              AND ts.hidden_reason IS NULL
              AND ts.provenance NOT LIKE 'human correction%'
              AND ts.id = (
                    SELECT MAX(v.id) FROM transcript_segments v
                     WHERE v.audio_segment_id = ts.audio_segment_id
                       AND v.start_utc = ts.start_utc
                       AND v.provenance NOT LIKE 'human correction%')
            ORDER BY ts.start_utc""",
        (
            source,
            (end + timedelta(seconds=tolerance_s)).isoformat(),
            (start - timedelta(seconds=tolerance_s)).isoformat(),
        ),
    ).fetchall()
    joined = " ".join(str(r[1]).strip() for r in rows if str(r[1]).strip())
    return joined or None


def load_cases(
    db: sqlite3.Connection,
    ingest: sqlite3.Connection,
    start: datetime,
    minutes: int,
    tolerance_s: float,
) -> tuple[list[Case], dict[str, int]]:
    """Corrections in the window whose block the two ranks DISAGREE about."""
    end = start + timedelta(minutes=minutes)
    rows = db.execute(
        """SELECT c.id, c.corrected_text, ts.start_utc, ts.end_utc, a.source_id
             FROM corrections c
             JOIN transcript_segments ts ON ts.id = c.transcript_segment_id
             JOIN audio_segments a ON a.id = ts.audio_segment_id
            WHERE ts.start_utc >= ? AND ts.end_utc <= ?
            ORDER BY ts.start_utc""",
        (start.isoformat(), end.isoformat()),
    ).fetchall()

    skipped = {"unrankable": 0, "ranks agree": 0, "an arm heard nothing": 0}
    cases: list[Case] = []
    for cid, truth, s_text, e_text, origin in rows:
        span_start, span_end = _utc(str(s_text)), _utc(str(e_text))
        chosen = winners(ingest, _block_of(span_start))
        if chosen is None:
            skipped["unrankable"] += 1
            continue
        raw_src, cal_src = chosen
        if raw_src == cal_src:
            skipped["ranks agree"] += 1
            continue
        raw_text = heard(db, raw_src, span_start, span_end, tolerance_s)
        cal_text = heard(db, cal_src, span_start, span_end, tolerance_s)
        if raw_text is None or cal_text is None:
            # Counted, not dropped: hiding these flatters whichever arm has a turn.
            skipped["an arm heard nothing"] += 1
            continue
        cases.append(
            Case(
                int(cid),
                str(truth),
                raw_src,
                cal_src,
                raw_text,
                cal_text,
                str(origin),
            )
        )
    return cases, skipped


def _report_by_origin(cases: list[Case]) -> None:
    """Split the medians by which microphone the human actually edited.

    ⚠⚠ **THE HEADLINE MEDIANS ABOVE ARE NOT A COMPARISON ON THEIR OWN.** Each
    correction flatters the arm it was typed over, so a one-sided set makes that
    arm win by construction and a balanced set averages the bias away into
    mush. What CAN be compared is cross against cross: each arm scored only
    against truth derived from the OTHER one.
    """
    from recall.wer import word_error_rate  # noqa: PLC0415

    origins = sorted({c.origin_source for c in cases})
    print("\nBY THE MICROPHONE THE HUMAN EDITED — self-scores are flattered:")
    cross: dict[str, list[float]] = {}
    for origin in origins:
        group = [c for c in cases if c.origin_source == origin]
        raw = statistics.median(word_error_rate(c.truth, c.raw_text) for c in group)
        cal = statistics.median(
            word_error_rate(c.truth, c.calibrated_text) for c in group
        )
        raw_src, cal_src = group[0].raw_source, group[0].calibrated_source

        def mark(src: str, origin: str = origin) -> str:
            return "SELF " if src == origin else "cross"

        print(
            f"  origin {origin:<10} n={len(group):<4} "
            f"raw({raw_src}) {raw:.3f} [{mark(raw_src)}]   "
            f"calibrated({cal_src}) {cal:.3f} [{mark(cal_src)}]"
        )
        if raw_src != origin:
            cross.setdefault(raw_src, []).append(raw)
        if cal_src != origin:
            cross.setdefault(cal_src, []).append(cal)

    if len(origins) < BOTH_ARMS:
        only = origins[0]
        print(
            f"\n⚠⚠ EVERY correction was typed over {only}, so that arm is "
            "flattered and the other cannot be compared without bias. This set "
            "cannot referee the ranks; correct a comparable number over the "
            "competing microphone."
        )
        return
    print("\n⭐ CROSS vs CROSS — each arm against truth derived from the other:")
    for src in sorted(cross):
        print(f"  {src:<10} {statistics.median(cross[src]):.3f}")
    print("  (lower is better, and neither side is scoring against its own words)")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--db", type=Path, required=True, help="fleet recall.sqlite")
    parser.add_argument(
        "--ingest", type=Path, required=True, help="fleet ingest.sqlite"
    )
    parser.add_argument("--start", required=True, help="window start, RFC3339")
    parser.add_argument("--minutes", type=int, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--tolerance", type=float, default=DEFAULT_TOLERANCE_S)
    args = parser.parse_args()

    from recall.wer import word_error_rate  # noqa: PLC0415 - keep import cost visible

    db = sqlite3.connect(f"file:{args.db}?mode=ro", uri=True)
    ingest = sqlite3.connect(f"file:{args.ingest}?mode=ro", uri=True)
    try:
        cases, skipped = load_cases(
            db, ingest, _utc(args.start), args.minutes, args.tolerance
        )
    finally:
        db.close()
        ingest.close()

    print(f"{len(cases)} scorable corrections; skipped {skipped}")
    print(f"cross-mic skew tolerance: {args.tolerance:.1f}s")
    if not cases:
        print(
            "\nNOTHING TO SCORE. If 'ranks agree' dominates, this window cannot "
            "referee the question — both arms would carry identical audio."
        )

    results: list[dict[str, object]] = []
    for case in cases:
        wer_raw = word_error_rate(case.truth, case.raw_text)
        wer_cal = word_error_rate(case.truth, case.calibrated_text)
        results.append(
            {
                "correction_id": case.correction_id,
                "truth": case.truth,
                "raw_source": case.raw_source,
                "calibrated_source": case.calibrated_source,
                "raw_text": case.raw_text,
                "calibrated_text": case.calibrated_text,
                "wer_raw": wer_raw,
                "wer_calibrated": wer_cal,
                "origin_source": case.origin_source,
            }
        )
        print(
            f"#{case.correction_id}: raw({case.raw_source}) {wer_raw:.2f}  "
            f"calibrated({case.calibrated_source}) {wer_cal:.2f}"
        )

    if results:
        raws = [float(r["wer_raw"]) for r in results]  # type: ignore[arg-type]
        cals = [float(r["wer_calibrated"]) for r in results]  # type: ignore[arg-type]
        better = sum(1 for r, c in zip(raws, cals, strict=True) if c < r)
        worse = sum(1 for r, c in zip(raws, cals, strict=True) if c > r)
        # Median: the mean moved 25x between two runs of identical audio when a
        # few hallucination loops landed in it (#1410).
        print(
            f"\nMEDIAN WER — raw: {statistics.median(raws):.3f}   "
            f"calibrated: {statistics.median(cals):.3f}   (n={len(results)})"
        )
        print(f"calibrated better on {better}, worse on {worse}")
        _report_by_origin(cases)
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(results, indent=2))
    print(f"wrote {args.out}")


if __name__ == "__main__":
    main()
