"""Naming household speech by voiceprint alone, refereed against human labels (#1711).

Two arms over the same human-labelled household turns (device sources only;
meetings keep pyannote):

  control   embed each labelled turn's own span and name it
  A         embed each Whisper segment of the clip and name it; a segment's truth
            is the label covering at least half of it

Both name with the production rule (`recalld::identify::match_one`: the person
whose best print is nearest; ported here and checked against the census's
0.913 on the same snapshot). A print made from audio overlapping the span being
scored is left out, or with `--leave-out clip` every print from that clip.

Every database is opened read-only (`immutable=1`: point it at a `.backup`
snapshot, never the live file).

Usage:
  # needs the ML environment, ffmpeg and HF_TOKEN; writes <work>/extract.jsonl
  <ml-env python> scripts/identify_referee.py extract \
      --db <snapshot> --clips <dir> --work <dir>
  python3 scripts/identify_referee.py score \
      --db <snapshot> --work <dir> [--leave-out clip]
"""

from __future__ import annotations

import argparse
import json
import math
import sqlite3
from collections import Counter, defaultdict
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path

LABELLED = """t.speaker_label IS NOT NULL AND t.speaker_label NOT LIKE 'SPEAKER%'
    AND t.hidden_reason IS NULL AND t.superseded_by IS NULL"""
MARGIN = 0.08


def instant(value: str) -> datetime:
    return datetime.fromisoformat(value.replace("Z", "+00:00"))


def open_ro(path: Path) -> sqlite3.Connection:
    return sqlite3.connect(f"file:{path}?immutable=1", uri=True)


def extract(db_path: Path, clips: Path, work: Path) -> None:
    """Transcribe each labelled household clip and embed its segments and turns."""
    from recall import shim_voices  # noqa: PLC0415 - the ML env, extract only
    from recall.asr import DEFAULT_MODEL, mlx_transcribe  # noqa: PLC0415 - same

    db = open_ro(db_path)
    out_path = work / "extract.jsonl"
    done = (
        {json.loads(line)["audio_id"] for line in out_path.open()}
        if out_path.exists()
        else set()
    )

    def embed(clip: Path, start: float, end: float) -> object:
        try:
            answer = shim_voices.handle(
                "embed", {"audio": str(clip), "start": start, "end": end}
            )
        except (ValueError, OSError, RuntimeError):
            return None
        return answer.get("vector") if isinstance(answer, dict) else None

    rows = db.execute(
        f"""SELECT DISTINCT a.id, a.path, a.start_utc FROM transcript_segments t
        JOIN audio_segments a ON a.id = t.audio_segment_id
        JOIN sources s ON s.id = a.source_id
        WHERE {LABELLED} AND s.kind IN ('coreaudio', 'tcp_pcm') ORDER BY a.id"""
    ).fetchall()
    with out_path.open("a") as out:
        for audio_id, path, start_utc in rows:
            if audio_id in done:
                continue
            clip = clips / str(path).rsplit("/", 1)[1]
            began = instant(start_utc)
            result = mlx_transcribe(
                clip,
                model=DEFAULT_MODEL,
                language=None,
                words=False,
                initial_prompt=None,
            )
            segments = [
                {"start": s.start, "end": s.end, "vector": embed(clip, s.start, s.end)}
                for s in result.segments
                if s.end > s.start
            ]
            turns = []
            for turn_id, s_utc, e_utc in db.execute(
                f"""SELECT t.id, t.start_utc, t.end_utc FROM transcript_segments t
                WHERE t.audio_segment_id = ? AND {LABELLED}""",
                (audio_id,),
            ):
                a = (instant(s_utc) - began).total_seconds()
                b = (instant(e_utc) - began).total_seconds()
                vector = embed(clip, max(a, 0.0), b) if b > a else None
                turns.append({"id": turn_id, "start": a, "end": b, "vector": vector})
            record = {"audio_id": audio_id, "segments": segments, "turns": turns}
            out.write(json.dumps(record) + "\n")
            out.flush()
            print(clip.name, len(segments), "segments", flush=True)


def unit(vector: list[float]) -> list[float]:
    norm = math.sqrt(sum(x * x for x in vector)) + 1e-12
    return [x / norm for x in vector]


@dataclass
class Print:
    person: str
    vector: list[float]
    source: tuple[int, float, float] | None


@dataclass
class Scored:
    right: bool
    seconds: float
    margin: float
    kind: str


def score(db_path: Path, work: Path, leave_out_clip: bool) -> None:
    """Score both arms and print the report."""
    db = open_ro(db_path)
    began = {
        i: instant(s) for i, s in db.execute("SELECT id, start_utc FROM audio_segments")
    }

    def span(
        aid: int | None, s: str | None, e: str | None
    ) -> tuple[int, float, float] | None:
        if aid is None or s is None or e is None or aid not in began:
            return None
        return (
            aid,
            (instant(s) - began[aid]).total_seconds(),
            (instant(e) - began[aid]).total_seconds(),
        )

    prints = [
        Print(person, unit(json.loads(vector)), span(ta, ts, te) or span(ca, cs, ce))
        for person, vector, ta, ts, te, ca, cs, ce in db.execute(
            """SELECT s.name, e.vector, t.audio_segment_id, t.start_utc, t.end_utc,
                      c.audio_segment_id, c.start_utc, c.end_utc
               FROM speaker_embeddings e JOIN speakers s ON s.id = e.speaker_id
               LEFT JOIN transcript_segments t ON t.id = e.source_segment_id
               LEFT JOIN corrections c ON c.id = e.source_correction_id"""
        )
    ]

    def name(
        vector: list[float], aid: int, a: float, b: float
    ) -> tuple[str | None, float]:
        e = unit(vector)
        best: dict[str, float] = {}
        for p in prints:
            src = p.source
            if (
                src
                and src[0] == aid
                and (leave_out_clip or (src[1] < b and src[2] > a))
            ):
                continue
            sim = sum(x * y for x, y in zip(e, p.vector, strict=True))
            best[p.person] = max(best.get(p.person, -9.0), sim)
        if not best:
            return None, 0.0
        ranked = sorted(best.values(), reverse=True)
        top = max(best, key=lambda person: best[person])
        return top, ranked[0] - (ranked[1] if len(ranked) > 1 else -1.0)

    label = dict(
        db.execute(
            "SELECT id, speaker_label FROM transcript_segments"
            " WHERE speaker_label IS NOT NULL"
        )
    )
    kind = dict(
        db.execute(
            "SELECT a.id, s.kind FROM audio_segments a"
            " JOIN sources s ON s.id = a.source_id"
        )
    )
    arms: dict[str, list[Scored]] = defaultdict(list)
    labelled_s = transcribed_s = 0.0
    for line in (work / "extract.jsonl").open():
        record = json.loads(line)
        aid = record["audio_id"]
        for t in record["turns"]:
            if t["vector"] is not None:
                guess, margin = name(t["vector"], aid, t["start"], t["end"])
                arms["control: labelled turn span"].append(
                    Scored(
                        guess == label[t["id"]],
                        t["end"] - t["start"],
                        margin,
                        kind[aid],
                    )
                )
        for s in record["segments"]:
            seconds = s["end"] - s["start"]
            transcribed_s += seconds
            cover: Counter[str] = Counter()
            for t in record["turns"]:
                overlap = min(t["end"], s["end"]) - max(t["start"], s["start"])
                cover[label[t["id"]]] += max(0.0, overlap)
            if not cover or s["vector"] is None:
                continue
            truth, overlap = cover.most_common(1)[0]
            if overlap < 0.5 * seconds:
                continue
            labelled_s += seconds
            guess, margin = name(s["vector"], aid, s["start"], s["end"])
            arms["A: Whisper segment"].append(
                Scored(guess == truth, seconds, margin, kind[aid])
            )

    print(
        f"{len(prints)} prints; labelled Whisper speech"
        f" {labelled_s:.0f}s of {transcribed_s:.0f}s"
    )
    for arm, rows in arms.items():
        report(arm, rows)


def report(arm: str, rows: list[Scored]) -> None:
    def rate(sub: list[Scored]) -> str:
        right = sum(r.right for r in sub)
        return f"{right}/{len(sub)} = {right / len(sub):.3f}"

    seconds = sum(r.seconds for r in rows)
    right_s = sum(r.seconds for r in rows if r.right)
    print(f"\n{arm}: {rate(rows)} by count, {right_s / seconds:.3f} by seconds")
    bands = [
        ("<1s", 0.0, 1.0),
        ("1-2s", 1.0, 2.0),
        ("2-5s", 2.0, 5.0),
        (">=5s", 5.0, 1e9),
    ]
    for band, lo, hi in bands:
        sub = [r for r in rows if lo <= r.seconds < hi]
        if sub:
            print(f"  {band:10} {rate(sub)}")
    for k in ("coreaudio", "tcp_pcm"):
        sub = [r for r in rows if r.kind == k]
        if sub:
            print(f"  {k:10} {rate(sub)}")
    sure = [r for r in rows if r.margin >= MARGIN]
    if sure:
        print(
            f"  margin >= {MARGIN}: {rate(sure)}"
            f" over {len(sure) / len(rows):.0%} of rows"
        )


def main() -> None:
    parser = argparse.ArgumentParser(
        description=__doc__.splitlines()[0] if __doc__ else ""
    )
    sub = parser.add_subparsers(dest="command", required=True)
    ex = sub.add_parser("extract")
    ex.add_argument("--db", type=Path, required=True)
    ex.add_argument("--clips", type=Path, required=True)
    ex.add_argument("--work", type=Path, required=True)
    sc = sub.add_parser("score")
    sc.add_argument("--db", type=Path, required=True)
    sc.add_argument("--work", type=Path, required=True)
    sc.add_argument("--leave-out", choices=["overlap", "clip"], default="overlap")
    args = parser.parse_args()
    if args.command == "extract":
        extract(args.db, args.clips, args.work)
    else:
        score(args.db, args.work, args.leave_out == "clip")


if __name__ == "__main__":
    main()
