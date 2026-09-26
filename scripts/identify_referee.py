"""Naming household speech by voiceprint alone, refereed against human labels (#1711).

Arms over the same human-labelled household turns (device sources only;
meetings keep pyannote):

  control   embed each labelled turn's own span and name it
  A         embed each Whisper segment of the clip and name it; a segment's truth
            is the label covering at least half of it
  A'        the same segments named by the mean of sliding windows inside them
            (`extract-windows`), which the next two cut:
  split     A' cut where the mean voice before and after differs most, if by
            more than `--threshold`, recursively
  ceiling   A' cut at the labelled edges: what a perfect detector would give

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
  <ml-env python> scripts/identify_referee.py extract-windows \
      --db <snapshot> --clips <dir> --work <dir>
  python3 scripts/identify_referee.py score \
      --db <snapshot> --work <dir> [--leave-out clip] \
      [--threshold T --shortest S --half even|odd]
"""

from __future__ import annotations

import argparse
import json
import math
import sqlite3
from collections import defaultdict
from collections.abc import Callable
from dataclasses import dataclass
from datetime import datetime
from itertools import pairwise
from pathlib import Path

LABELLED = """t.speaker_label IS NOT NULL AND t.speaker_label NOT LIKE 'SPEAKER%'
    AND t.hidden_reason IS NULL AND t.superseded_by IS NULL"""
MARGIN = 0.08
CONTROL = "control: labelled turn span"
ORACLE_EMBED = "ceiling+: cut at the labelled edges, each piece embedded"
SPLIT_EMBED = "split+: cut where the voice changes, each piece embedded"
MEAN = "A': Whisper segment, window mean"
ORACLE = "ceiling: A' cut at the labelled edges"
SPLIT = "split: A' cut where the voice changes"
WHISPER = "A: Whisper segment"
PYANNOTE = "pyannote: aligned turn, cluster voiceprint"


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


def extract_pyannote(db_path: Path, clips: Path, work: Path) -> None:
    """Run the production diarize request and a word-timed transcription per
    clip, stored as the runner stores job results, for `align_referee`."""
    from recall import shim, shim_asr, shim_voices  # noqa: PLC0415 - the ML env only

    db = open_ro(db_path)
    out_path = work / "pyannote.jsonl"
    done = (
        {json.loads(line)["audio_id"] for line in out_path.open()}
        if out_path.exists()
        else set()
    )
    rows = db.execute(
        f"""SELECT DISTINCT a.id, a.path FROM transcript_segments t
        JOIN audio_segments a ON a.id = t.audio_segment_id
        JOIN sources s ON s.id = a.source_id
        WHERE {LABELLED} AND s.kind IN ('coreaudio', 'tcp_pcm') ORDER BY a.id"""
    ).fetchall()
    with out_path.open("a") as out:
        for audio_id, path in rows:
            if audio_id in done:
                continue
            clip = str(clips / str(path).rsplit("/", 1)[1])
            record = {"audio_id": audio_id}
            for key, op, handle, args in (
                ("transcribe", "transcribe", shim_asr.handle, {"words": True}),
                ("diarize", "diarize", shim_voices.handle, {"embed": True}),
            ):
                try:
                    result = handle(op, {"audio": clip, **args})
                    # as the worker replies: NaN becomes null (`shim.finite`)
                    record[key] = {"ok": True, "result": shim.finite(result)}
                except (ValueError, OSError, RuntimeError) as err:
                    record[key] = {"ok": False, "error": str(err)}
            out.write(json.dumps(record) + "\n")
            out.flush()
            print(Path(clip).name, flush=True)


WINDOW_S = 1.5
HOP_S = 0.5


def extract_windows(db_path: Path, clips: Path, work: Path) -> None:
    """Embed sliding windows over each labelled clip, once, so split rules can be
    tried offline. The production model, run in pyannote's sliding mode on the
    decode production uses."""
    import os  # noqa: PLC0415 - extract only

    from pyannote.audio import Inference, Model  # noqa: PLC0415 - the ML env only

    from recall.speakerid import (  # noqa: PLC0415 - same
        _EMBED_RATE,  # pyright: ignore[reportPrivateUsage]
        _decode_mono,  # pyright: ignore[reportPrivateUsage]
    )

    model = Model.from_pretrained(
        "pyannote/embedding", token=os.environ.get("HF_TOKEN")
    )
    if model is None:
        msg = "could not load pyannote/embedding (HF token/terms?)"
        raise RuntimeError(msg)
    sliding = Inference(model, window="sliding", duration=WINDOW_S, step=HOP_S)
    db = open_ro(db_path)
    out_path = work / "windows.jsonl"
    done = (
        {json.loads(line)["audio_id"] for line in out_path.open()}
        if out_path.exists()
        else set()
    )
    rows = db.execute(
        f"""SELECT DISTINCT a.id, a.path FROM transcript_segments t
        JOIN audio_segments a ON a.id = t.audio_segment_id
        JOIN sources s ON s.id = a.source_id
        WHERE {LABELLED} AND s.kind IN ('coreaudio', 'tcp_pcm') ORDER BY a.id"""
    ).fetchall()
    with out_path.open("a") as out:
        for audio_id, path in rows:
            if audio_id in done:
                continue
            clip = clips / str(path).rsplit("/", 1)[1]
            feature = sliding(
                {"waveform": _decode_mono(clip), "sample_rate": _EMBED_RATE}
            )
            windows = [
                {
                    "start": round(float(frame.start), 3),
                    "end": round(float(frame.end), 3),
                    "vector": [round(float(x), 5) for x in vector],
                }
                for frame, vector in feature
            ]
            out.write(json.dumps({"audio_id": audio_id, "windows": windows}) + "\n")
            out.flush()
            print(clip.name, len(windows), "windows", flush=True)


THRESHOLDS = (0.3, 0.45, 0.6, 0.75, 0.9)
EMBEDDED = "pieces.jsonl"


def span_key(aid: int, a: float, b: float) -> tuple[int, float, float]:
    return (aid, round(a, 3), round(b, 3))


def load_windows(work: Path) -> dict[int, list[Window]]:
    out: dict[int, list[Window]] = {}
    for line in (work / "windows.jsonl").open():
        record = json.loads(line)
        out[record["audio_id"]] = [
            (w["start"], w["end"], w["vector"])
            for w in record["windows"]
            if usable(w["vector"]) is not None
        ]
    return out


def candidate_pieces(work: Path, shortest: float) -> set[tuple[int, float, float]]:
    """Every piece an arm could name: each Whisper segment cut at the labelled
    edges, and cut by the detector at each of `THRESHOLDS`."""
    windows = load_windows(work)
    spans: set[tuple[int, float, float]] = set()
    for line in (work / "extract.jsonl").open():
        record = json.loads(line)
        aid = record["audio_id"]
        edges = [x for t in record["turns"] for x in (t["start"], t["end"])]
        for seg in record["segments"]:
            a, b = seg["start"], seg["end"]
            cuts = [edges] + [
                voice_changes(windows.get(aid, []), a, b, t, shortest)
                for t in THRESHOLDS
            ]
            for at in cuts:
                spans.update(span_key(aid, pa, pb) for pa, pb in pieces(a, b, at))
    return spans


def extract_pieces(db_path: Path, clips: Path, work: Path, shortest: float) -> None:
    """Embed every candidate piece as production embeds a span, once each."""
    from recall import shim_voices  # noqa: PLC0415 - the ML env only

    paths = dict(open_ro(db_path).execute("SELECT id, path FROM audio_segments"))
    out_path = work / EMBEDDED
    done = set()
    if out_path.exists():
        for line in out_path.open():
            r = json.loads(line)
            done.add(span_key(r["audio_id"], r["start"], r["end"]))
    todo = sorted(candidate_pieces(work, shortest) - done)
    print(len(todo), "pieces to embed", flush=True)
    with out_path.open("a") as out:
        for n, (aid, a, b) in enumerate(todo, 1):
            clip = clips / str(paths[aid]).rsplit("/", 1)[1]
            try:
                answer = shim_voices.handle(
                    "embed", {"audio": str(clip), "start": a, "end": b}
                )
                vector = answer.get("vector") if isinstance(answer, dict) else None
            except (ValueError, OSError, RuntimeError):
                vector = None
            record = {"audio_id": aid, "start": a, "end": b, "vector": vector}
            out.write(json.dumps(record) + "\n")
            if n % 100 == 0:
                out.flush()
                print(n, "embedded", flush=True)


def usable(vector: list[float] | None) -> list[float] | None:
    """A span too short to embed comes back NaN, which ties every person: it is
    unnamed, not a coin-flip."""
    if vector is None or not all(math.isfinite(x) for x in vector):
        return None
    return vector


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


def load_prints(db: sqlite3.Connection) -> list[Print]:
    """Every enrolled print, with the clip span it was made from where known."""
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

    return [
        Print(person, unit(json.loads(vector)), span(ta, ts, te) or span(ca, cs, ce))
        for person, vector, ta, ts, te, ca, cs, ce in db.execute(
            """SELECT s.name, e.vector, t.audio_segment_id, t.start_utc, t.end_utc,
                      c.audio_segment_id, c.start_utc, c.end_utc
               FROM speaker_embeddings e JOIN speakers s ON s.id = e.speaker_id
               LEFT JOIN transcript_segments t ON t.id = e.source_segment_id
               LEFT JOIN corrections c ON c.id = e.source_correction_id"""
        )
    ]


Window = tuple[float, float, list[float]]


def mean_of(windows: list[Window], a: float, b: float) -> list[float] | None:
    """The mean unit vector of the windows centred in [a, b), or of the window
    centred nearest when the span is shorter than a hop."""
    inside = [w for w in windows if a <= (w[0] + w[1]) / 2 < b]
    if not inside and windows:
        mid = (a + b) / 2
        inside = [min(windows, key=lambda w: abs((w[0] + w[1]) / 2 - mid))]
    if not inside:
        return None
    return [sum(xs) / len(inside) for xs in zip(*(w[2] for w in inside), strict=True)]


def cosine(u: list[float], v: list[float]) -> float:
    return sum(x * y for x, y in zip(unit(u), unit(v), strict=True))


def voice_changes(
    windows: list[Window], a: float, b: float, threshold: float, shortest: float
) -> list[float]:
    """Cut points inside [a, b), by binary segmentation: cut where the mean voice
    before and after differ most, if by more than `threshold` in cosine distance,
    and recurse. No piece shorter than `shortest` seconds."""
    inside = [w for w in windows if a <= (w[0] + w[1]) / 2 < b]
    best: tuple[float, float] | None = None
    for k in range(1, len(inside)):
        cut = (inside[k - 1][1] + inside[k][0]) / 2
        if cut - a < shortest or b - cut < shortest:
            continue
        left = mean_of(inside[:k], a, cut)
        right = mean_of(inside[k:], cut, b)
        if left is None or right is None:
            continue
        distance = 1.0 - cosine(left, right)
        if best is None or distance > best[0]:
            best = (distance, cut)
    if best is None or best[0] <= threshold:
        return []
    cut = best[1]
    return [
        *voice_changes(windows, a, cut, threshold, shortest),
        cut,
        *voice_changes(windows, cut, b, threshold, shortest),
    ]


def pieces(a: float, b: float, cuts: list[float]) -> list[tuple[float, float]]:
    edges = [a, *sorted(c for c in cuts if a < c < b), b]
    return list(pairwise(edges))


ScoreUnit = Callable[[str, int, float, float, list[float] | None], None]


def score_windows(
    work: Path,
    truth: dict[int, list[tuple[float, float, str]]],
    segments: dict[int, list[tuple[float, float]]],
    split: Split,
    score_unit: ScoreUnit,
) -> None:
    """The window arms: each Whisper segment whole, cut by the detector, and cut
    at the labelled edges, every piece named by the mean of its windows."""
    windowed = work / "windows.jsonl"
    if not windowed.exists():
        return
    embedded: dict[tuple[int, float, float], list[float] | None] = {}
    if (work / EMBEDDED).exists():
        for line in (work / EMBEDDED).open():
            r = json.loads(line)
            embedded[span_key(r["audio_id"], r["start"], r["end"])] = r["vector"]
    for line in windowed.open():
        record = json.loads(line)
        aid = record["audio_id"]
        if aid not in truth:
            continue
        windows: list[Window] = [
            (w["start"], w["end"], w["vector"])
            for w in record["windows"]
            if usable(w["vector"]) is not None
        ]
        edges = [x for ta, tb, _ in truth[aid] for x in (ta, tb)]
        for a, b in segments[aid]:
            detected = voice_changes(windows, a, b, split.threshold, split.shortest)
            cuts = {MEAN: [], ORACLE: edges, SPLIT: detected}
            for arm, at in cuts.items():
                for pa, pb in pieces(a, b, at):
                    score_unit(arm, aid, pa, pb, mean_of(windows, pa, pb))
            if embedded:
                for arm, at in ((ORACLE_EMBED, edges), (SPLIT_EMBED, detected)):
                    for pa, pb in pieces(a, b, at):
                        vector = embedded.get(span_key(aid, pa, pb))
                        score_unit(arm, aid, pa, pb, vector)


def score_aligned(
    work: Path, truth: dict[int, list[tuple[float, float, str]]], score_unit: ScoreUnit
) -> None:
    """The pyannote arm: each aligned turn, named by its cluster's voiceprint."""
    aligned = work / "aligned.jsonl"
    if aligned.exists():
        for line in aligned.open():
            record = json.loads(line)
            aid = record["audio_id"]
            if aid not in truth:
                continue
            voice = {v["speaker"]: v["vector"] for v in record["voices"]}
            for t in record["turns"]:
                vector = voice.get(t["speaker"])
                score_unit(PYANNOTE, aid, t["start"], t["end"], vector)


@dataclass
class Split:
    """The change detector's settings, and which clips to score: tune on one
    half by audio id, report on the other."""

    threshold: float = 0.3
    shortest: float = 1.0
    half: str = "all"


def score(db_path: Path, work: Path, leave_out_clip: bool, split: Split) -> None:
    """Score every arm and print the report."""
    db = open_ro(db_path)
    prints = load_prints(db)

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
            sim = math.sumprod(e, p.vector)
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
    # Per arm, per clip: the spans it named, for the labelled-seconds metric.
    named: dict[str, dict[int, list[tuple[float, float, str | None]]]] = defaultdict(
        lambda: defaultdict(list)
    )
    truth: dict[int, list[tuple[float, float, str]]] = {}

    def unit_truth(aid: int, a: float, b: float) -> str | None:
        cover: defaultdict[str, float] = defaultdict(float)
        for ta, tb, who in truth[aid]:
            cover[who] += max(0.0, min(tb, b) - max(ta, a))
        if not cover:
            return None
        who = max(cover, key=lambda person: cover[person])
        return who if cover[who] >= 0.5 * (b - a) else None

    def score_unit(
        arm: str, aid: int, a: float, b: float, vector: list[float] | None
    ) -> None:
        vector = usable(vector)
        guess = name(vector, aid, a, b) if vector is not None else (None, 0.0)
        named[arm][aid].append((a, b, guess[0]))
        want = unit_truth(aid, a, b)
        if want is not None and vector is not None:
            arms[arm].append(Scored(guess[0] == want, b - a, guess[1], kind[aid]))

    def wanted(aid: int) -> bool:
        return split.half == "all" or (aid % 2 == 0) == (split.half == "even")

    segments: dict[int, list[tuple[float, float]]] = {}
    for line in (work / "extract.jsonl").open():
        record = json.loads(line)
        aid = record["audio_id"]
        if not wanted(aid):
            continue
        truth[aid] = [(t["start"], t["end"], label[t["id"]]) for t in record["turns"]]
        segments[aid] = [(seg["start"], seg["end"]) for seg in record["segments"]]
        for t in record["turns"]:
            score_unit(CONTROL, aid, t["start"], t["end"], t["vector"])
        for seg in record["segments"]:
            score_unit(WHISPER, aid, seg["start"], seg["end"], seg["vector"])
    score_windows(work, truth, segments, split, score_unit)
    score_aligned(work, truth, score_unit)

    labelled_s = sum(b - a for turns in truth.values() for a, b, _ in turns)
    print(f"{len(prints)} prints; {len(truth)} clips; {labelled_s:.0f}s labelled")
    for arm, rows in arms.items():
        report(arm, rows)
        covered, right = named_right(truth, named[arm])
        print(
            f"  labelled speech: {covered / labelled_s:.0%} covered,"
            f" {right / labelled_s:.0%} named right"
        )


def named_right(
    truth: dict[int, list[tuple[float, float, str]]],
    spans: dict[int, list[tuple[float, float, str | None]]],
) -> tuple[float, float]:
    """Seconds of labelled speech an arm named at all, and named right."""
    covered = right = 0.0
    for aid, turns in truth.items():
        for ta, tb, who in turns:
            for a, b, guess in spans.get(aid, []):
                overlap = max(0.0, min(tb, b) - max(ta, a))
                covered += overlap
                right += overlap if guess == who else 0.0
    return covered, right


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
    ep = sub.add_parser("extract-pyannote")
    ep.add_argument("--db", type=Path, required=True)
    ep.add_argument("--clips", type=Path, required=True)
    ep.add_argument("--work", type=Path, required=True)
    ew = sub.add_parser("extract-windows")
    ew.add_argument("--db", type=Path, required=True)
    ew.add_argument("--clips", type=Path, required=True)
    ew.add_argument("--work", type=Path, required=True)
    xp = sub.add_parser("extract-pieces")
    xp.add_argument("--db", type=Path, required=True)
    xp.add_argument("--clips", type=Path, required=True)
    xp.add_argument("--work", type=Path, required=True)
    xp.add_argument("--shortest", type=float, default=Split.shortest)
    sc = sub.add_parser("score")
    sc.add_argument("--db", type=Path, required=True)
    sc.add_argument("--work", type=Path, required=True)
    sc.add_argument("--leave-out", choices=["overlap", "clip"], default="overlap")
    sc.add_argument("--threshold", type=float, default=Split.threshold)
    sc.add_argument("--shortest", type=float, default=Split.shortest)
    sc.add_argument("--half", choices=["all", "even", "odd"], default="all")
    args = parser.parse_args()
    if args.command == "extract":
        extract(args.db, args.clips, args.work)
    elif args.command == "extract-pyannote":
        extract_pyannote(args.db, args.clips, args.work)
    elif args.command == "extract-windows":
        extract_windows(args.db, args.clips, args.work)
    elif args.command == "extract-pieces":
        extract_pieces(args.db, args.clips, args.work, args.shortest)
    else:
        score(
            args.db,
            args.work,
            args.leave_out == "clip",
            Split(args.threshold, args.shortest, args.half),
        )


if __name__ == "__main__":
    main()
