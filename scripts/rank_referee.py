"""Raw vs calibrated room selection, refereed against human corrections.

#1461's referee, and it did not exist until now. `fusion_bakeoff.py` scores the
FUSED rendering against one reference mic — a different question — and it reads a
local `--db`, which since `sync` was deleted means the Mac's copy, frozen at
2026-07-11. Corrections are made on the FLEET.

For each correction this asks which microphone each RANK would have chosen for
the block it falls in, takes what THAT MICROPHONE ACTUALLY TRANSCRIBED there, and
scores both against the corrected text. The verdict is the median.

⚠ **It compares STORED TRANSCRIPTS, not re-transcribed audio slices, and that is
the whole design.** The first version sliced the corrected span out of each
source's clip and ran the model on both. It scored every case 1.00/1.00, and
looking at why produced the finding: the correction's span comes from ONE
source's timeline, and the microphones do not share a clock. On 2026-06-19
19:05:18 the usb mic heard "New practical technique. Let's see if it goes right."
while pixel9 heard the truth — "Nu werkt die goed denk ik." — 3.7 SECONDS LATER
on its own clock. Slicing usb's span out of pixel9's clip lands on the wrong
audio, so the arm that was RIGHT scored as a total miss. See
`project_recall_phone_clock_skew`: never compare cross-mic timestamps as if they
share a reference.

⚠ Matching is therefore by OVERLAP inside a tolerance, not by equality, and the
tolerance is stated in the output. Widen it and neighbouring speech starts to
qualify; narrow it and real matches are lost. Read the n, not only the medians.

⚠ **A block where both ranks choose the same microphone is SKIPPED, not scored.**
Both arms would carry identical audio and tie, which is how the June window
"refereed" this twice and answered nothing: usb wins there under both rules. A
tie between identical inputs is not evidence.

⚠ **It reports the MEDIAN.** Hallucination loops (#1410) moved the mean 25x
between two runs of the SAME audio while the median did not move at all.

⚠ Both arms' transcripts were produced with the same vocabulary biasing, so the
comparison is symmetric — but neither is the bare model, so these numbers are not
comparable with score-asr's.

Non-destructive: every database is opened read-only; only the report is written.

Usage (no model needed — it reads what the pipeline already wrote):
  python3 scripts/rank_referee.py \
      --db /tmp/fleet-recall.sqlite --ingest /tmp/fleet-ingest.sqlite \
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

DEFAULT_TOLERANCE_S = 6.0
"""How far a source's turn may sit from the corrected span and still be its match.

⚠ Sized from a MEASURED skew, not chosen: pixel9 ran 3.7 s behind usb on
2026-06-19. Six seconds covers that with headroom and is stated in the report so
a reader can see what it admitted.
"""


@dataclass(frozen=True)
class Case:
    correction_id: int
    truth: str
    raw_source: str
    calibrated_source: str
    raw_text: str
    calibrated_text: str


def _utc(text: str) -> datetime:
    parsed = datetime.fromisoformat(text.replace("Z", "+00:00"))
    return parsed if parsed.tzinfo else parsed.replace(tzinfo=UTC)


def _block_of(when: datetime) -> str:
    """The UTC-aligned minute a time falls in, as the builder stamps it."""
    return when.replace(second=0, microsecond=0).strftime("%Y-%m-%dT%H:%M:%SZ")


def winners(ingest: sqlite3.Connection, block: str) -> tuple[str, str] | None:
    """(raw winner, calibrated winner) for a block, or None if unrankable.

    Read from the verdict the builder RECORDED rather than re-derived, so this
    scores the choice production would have made at the time.
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

    Every CURRENT turn whose span overlaps the corrected one, joined in time
    order — a single utterance is often several turns on one microphone and one
    on another, so taking the nearest turn alone would score a fragment against a
    sentence.

    ⚠⚠ **HUMAN CORRECTIONS ARE EXCLUDED, and this is the difference between a
    measurement and a tautology.** A correction is stored as a NEW TURN with
    provenance `human correction of #N` which SUPERSEDES the machine's. Take the
    current turn and the hypothesis IS the truth: both arms scored 0.00 and were
    identical on every case, which is what caught it. The hypothesis has to be
    what the MICROPHONE heard, so corrections are filtered out and the machine's
    own turn — superseded though it now is — is what gets scored.

    ⚠ And only the LATEST machine version per moment. The archive keeps every
    re-transcription, so without that a moment contributes its old text and its
    new one and the arm is scored against the same sentence twice — WER above 1.0
    for both arms, which is how THAT was caught.
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
        """SELECT c.id, c.corrected_text, ts.start_utc, ts.end_utc
             FROM corrections c
             JOIN transcript_segments ts ON ts.id = c.transcript_segment_id
            WHERE ts.start_utc >= ? AND ts.end_utc <= ?
            ORDER BY ts.start_utc""",
        (start.isoformat(), end.isoformat()),
    ).fetchall()

    skipped = {"unrankable": 0, "ranks agree": 0, "an arm heard nothing": 0}
    cases: list[Case] = []
    for cid, truth, s_text, e_text in rows:
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
            # ⚠ Counted, not silently dropped: an arm with no turn at all is a
            # real outcome (that microphone contributed nothing), and hiding it
            # would flatter whichever arm does have one.
            skipped["an arm heard nothing"] += 1
            continue
        cases.append(Case(int(cid), str(truth), raw_src, cal_src, raw_text, cal_text))
    return cases, skipped


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
        # ⚠ MEDIAN. The mean moved 25x between two runs of identical audio when a
        # few hallucination loops landed in it (#1410); the median did not move.
        print(
            f"\nMEDIAN WER — raw: {statistics.median(raws):.3f}   "
            f"calibrated: {statistics.median(cals):.3f}   (n={len(results)})"
        )
        print(f"calibrated better on {better}, worse on {worse}")
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(results, indent=2))
    print(f"wrote {args.out}")


if __name__ == "__main__":
    main()
