"""Room stream vs per-microphone transcripts, refereed against human corrections.

#1388 asks whether transcribing the ROOM once can replace transcribing every
microphone. Its loop, turn-count and character proxies have each been read wrong
once; the only measurement that settles it is WER against text a person wrote.

⚠ **The room arm is not in `transcript_segments`.** The room turn writer is off,
so the room's words live only in the stored `transcribe-room` job result. This
reads that, which is why the comparison is possible at all without switching the
writer on first.

Three constraints inherited from `rank_referee.py`, each of which produced a
wrong answer when missed:

  - Compare STORED transcripts, never re-transcribed slices. The mics do not
    share a clock — pixel9 ran 3.7 s behind usb — so matching is by overlap
    within a tolerance.
  - Exclude HUMAN CORRECTIONS from the hypothesis. A correction is stored as a
    turn that supersedes the machine's, so reading the current turn makes the
    hypothesis identical to the truth.
  - Take only the LATEST machine version per moment; the archive keeps every
    re-transcription.

⚠⚠ **THIS CANNOT SETTLE #1388 ON THE EXISTING CORRECTIONS, and the reason is
structural.** A correction is an EDIT OF THE MICROPHONE'S OWN TEXT: the person
read what one microphone said and fixed the words that were wrong. So the
reference shares its vocabulary and phrasing with the per-mic hypothesis and is
independent of the room's. The per-mic arm would win even if the two were
equally accurate, and the size of that advantage cannot be recovered from this
data. Fresh ground truth written against AUDIO — #1461 — is therefore necessary
rather than merely convenient.

⚠ 62% of this archive's corrections changed no text at all: 456 of 471 carry a
speaker label, so most are attributions. Without `--changed-only` the per-mic arm
scores a median 0.0 by construction, because for those cases the truth IS its own
text.

⚠ And one of its own: the two arms are reported SEPARATELY for blocks the
correction's own microphone won and blocks it did not. Where it won, both arms
carry the SAME AUDIO and the comparison is of pipelines; where it lost, the room
is a different microphone and the comparison includes the selection. Pooling them
answers neither question.

Every database is opened read-only; only the report is written.

Usage:
  python3 scripts/room_referee.py \
      --db <fleet recall.sqlite> --ingest <fleet ingest.sqlite> --out /tmp/room.json
"""

from __future__ import annotations

import argparse
import json
import sqlite3
import statistics
import sys
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "src"))

from recall.wer import normalize_text, word_error_rate

DEFAULT_TOLERANCE_S = 6.0
"""Cross-mic skew a turn may sit away and still match, sized from a measured 3.7 s."""

BLOCK_S = 60


def infix_error_rate(reference: str, hypothesis: str) -> float:
    """Error rate of the BEST-MATCHING span of `hypothesis` against `reference`.

    ⚠ **Plain WER cannot compare arms of different granularity, and reading it
    that way is the trap this file exists inside.** A correction is a short
    utterance; both arms are gathered over its span plus a skew tolerance, so
    both capture surrounding speech — and the room's segments are coarser, so it
    captures MORE. Measured that way the room scored a median 4.33 against the
    microphone's 2.33, which is almost entirely a statement about window size.

    Levenshtein with a FREE prefix and suffix: the alignment may start and end
    anywhere in the hypothesis at no cost, so surrounding words neither help nor
    hurt, and what is left is whether the corrected words are actually there.
    """
    ref = normalize_text(reference).split()
    hyp = normalize_text(hypothesis).split()
    if not ref:
        return 0.0
    if not hyp:
        return 1.0
    previous = [0] * (len(hyp) + 1)
    for i, ref_word in enumerate(ref, start=1):
        current = [i]
        for j, hyp_word in enumerate(hyp, start=1):
            cost = 0 if ref_word == hyp_word else 1
            current.append(
                min(previous[j] + 1, current[j - 1] + 1, previous[j - 1] + cost)
            )
        previous = current
    return min(previous) / len(ref)


@dataclass(frozen=True)
class Case:
    correction_id: int
    source: str
    winner: str
    truth: str
    mic_text: str
    room_text: str


def _utc(text: str) -> datetime:
    parsed = datetime.fromisoformat(text.replace("Z", "+00:00"))
    return parsed if parsed.tzinfo else parsed.replace(tzinfo=UTC)


def _block_of(when: datetime) -> str:
    """The UTC-aligned minute a time falls in, as the builder stamps it."""
    return when.replace(second=0, microsecond=0).strftime("%Y-%m-%dT%H:%M:%SZ")


def mic_heard(
    db: sqlite3.Connection, source: str, start: datetime, end: datetime, tol: float
) -> str | None:
    """What `source` itself transcribed over the span, machine turns only."""
    rows = db.execute(
        """SELECT ts.text FROM transcript_segments ts
             JOIN audio_segments a ON a.id = ts.audio_segment_id
            WHERE a.source_id = ?
              AND ts.start_utc <= ? AND ts.end_utc >= ?
              AND ts.hidden_reason IS NULL
              AND ts.asr_model <> 'human'
              AND (ts.provenance IS NULL OR ts.provenance NOT LIKE 'human correction%')
              AND ts.id = (
                    SELECT MAX(v.id) FROM transcript_segments v
                     WHERE v.audio_segment_id = ts.audio_segment_id
                       AND v.start_utc = ts.start_utc
                       AND (v.provenance IS NULL
                            OR v.provenance NOT LIKE 'human correction%'))
            ORDER BY ts.start_utc""",
        (
            source,
            (end + timedelta(seconds=tol)).isoformat(),
            (start - timedelta(seconds=tol)).isoformat(),
        ),
    ).fetchall()
    joined = " ".join(str(r[0]).strip() for r in rows if str(r[0]).strip())
    return joined or None


def room_heard(
    ingest: sqlite3.Connection, start: datetime, end: datetime, tol: float
) -> tuple[str, str] | None:
    """(winner, what the ROOM said) over the span, from the stored job result.

    ⚠ Segment offsets in the result are relative to the block's own start, so the
    span is converted into block-relative seconds before overlap is tested.
    """
    block = _block_of(start)
    row = ingest.execute(
        """SELECT r.winner, j.result FROM room_blocks r
             JOIN jobs j ON j.filename = r.filename
                        AND j.kind = 'transcribe-room' AND j.result IS NOT NULL
            WHERE r.start_utc = ? AND r.verdict LIKE 'built%'""",
        (block,),
    ).fetchone()
    if row is None:
        return None
    winner, raw = row
    try:
        body = json.loads(raw).get("result") or {}
    except (json.JSONDecodeError, AttributeError):
        return None
    block_start = _utc(block)
    lo = (start - block_start).total_seconds() - tol
    hi = (end - block_start).total_seconds() + tol
    said = [
        str(s.get("text", "")).strip()
        for s in body.get("segments", [])
        if s.get("start") is not None
        and s.get("end") is not None
        and float(s["end"]) > lo
        and float(s["start"]) < hi
    ]
    joined = " ".join(t for t in said if t)
    return (str(winner), joined) if joined else None


def load_cases(
    db: sqlite3.Connection, ingest: sqlite3.Connection, tol: float, changed_only: bool
) -> tuple[list[Case], dict[str, int]]:
    skipped: dict[str, int] = {
        "no_mic_text": 0,
        "no_room_text": 0,
        "a_meeting": 0,
        "text_unchanged": 0,
    }
    cases: list[Case] = []
    rows = db.execute(
        """SELECT c.id, c.start_utc, c.end_utc, c.corrected_text, a.source_id,
                  c.original_text
             FROM corrections c JOIN audio_segments a ON a.id = c.audio_segment_id
            WHERE c.corrected_text <> '' ORDER BY c.start_utc"""
    ).fetchall()
    for cid, raw_start, raw_end, truth, source, original in rows:
        if changed_only and normalize_text(str(original or "")) == normalize_text(
            str(truth)
        ):
            skipped["text_unchanged"] += 1
            continue
        if str(source).startswith("meeting-"):
            skipped["a_meeting"] += 1
            continue
        start, end = _utc(raw_start), _utc(raw_end)
        mic = mic_heard(db, str(source), start, end, tol)
        if mic is None:
            skipped["no_mic_text"] += 1
            continue
        room = room_heard(ingest, start, end, tol)
        if room is None:
            skipped["no_room_text"] += 1
            continue
        winner, room_text = room
        cases.append(Case(int(cid), str(source), winner, str(truth), mic, room_text))
    return cases, skipped


def report(cases: list[Case]) -> dict[str, object]:
    def arm(group: list[Case]) -> dict[str, object]:
        if not group:
            return {"n": 0}
        mic = [infix_error_rate(c.truth, c.mic_text) for c in group]
        room = [infix_error_rate(c.truth, c.room_text) for c in group]
        better = sum(1 for m, r in zip(mic, room, strict=True) if r < m)
        worse = sum(1 for m, r in zip(mic, room, strict=True) if r > m)
        return {
            "n": len(group),
            "median_infix_err_per_mic": round(statistics.median(mic), 4),
            "median_infix_err_room": round(statistics.median(room), 4),
            "room_better": better,
            "room_worse": worse,
            "tied": len(group) - better - worse,
            # ⚠ Kept only to show how far the window confound moves it.
            "median_plain_wer_per_mic": round(
                statistics.median(
                    [word_error_rate(c.truth, c.mic_text) for c in group]
                ),
                4,
            ),
            "median_plain_wer_room": round(
                statistics.median(
                    [word_error_rate(c.truth, c.room_text) for c in group]
                ),
                4,
            ),
        }

    same = [c for c in cases if c.winner == c.source]
    other = [c for c in cases if c.winner != c.source]
    return {
        "same_audio_the_mic_won_the_block": arm(same),
        "different_audio_another_mic_won": arm(other),
        "pooled_do_not_quote_alone": arm(cases),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--db", required=True, type=Path)
    parser.add_argument("--ingest", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--tolerance", type=float, default=DEFAULT_TOLERANCE_S)
    parser.add_argument(
        "--changed-only",
        action="store_true",
        help=(
            "Keep only corrections whose TEXT changed. ⚠ 62%% of this archive's "
            "corrections are speaker labels that left the words alone, and for "
            "those the truth IS the microphone's own text, so the per-mic arm "
            "scores zero by construction."
        ),
    )
    args = parser.parse_args()

    db = sqlite3.connect(f"file:{args.db}?mode=ro", uri=True)
    ingest = sqlite3.connect(f"file:{args.ingest}?mode=ro", uri=True)
    cases, skipped = load_cases(db, ingest, args.tolerance, args.changed_only)
    out = {
        "tolerance_s": args.tolerance,
        "cases": len(cases),
        "skipped": skipped,
        "arms": report(cases),
    }
    args.out.write_text(json.dumps(out, indent=2) + "\n")
    print(json.dumps(out["arms"], indent=2))
    print(f"\n{len(cases)} cases, skipped {skipped}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
