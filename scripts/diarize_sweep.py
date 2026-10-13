"""How pyannote's clustering threshold separates the speakers people named.

For each clip, every naming still in force (a person's Speaker act) is put on
the diarized voice under most of its span. Printed per clip and threshold:
namings on a voice mostly named as someone else (two speakers merged),
purity (named seconds on their voice's majority name), and how many voices
the clip has. `production` scores the stored diarization, the control a
default run must match.

Names are compared, never printed: the output is safe to paste.

Usage: python scripts/diarize_sweep.py LAB CLIP_ID... [--thresholds 0.5 0.6]
  LAB holds recall.sqlite, ingest.sqlite and ingest/<source>/<file>.
"""

from __future__ import annotations

import argparse
import json
import re
import sqlite3
import sys
from collections import Counter, defaultdict
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

Turn = tuple[str, float, float]


def in_force(edits: list[tuple[int, dict[str, Any]]]) -> set[int]:
    """Edit ids not taken back: a retraction removes its target, and a
    retracted retraction restores it. Retractions follow their targets."""
    retracts: dict[int, list[int]] = defaultdict(list)
    for eid, act in edits:
        if "Retract" in act:
            retracts[act["Retract"]["edit"]].append(eid)
    alive: dict[int, bool] = {}
    for eid, _ in sorted(edits, reverse=True):
        alive[eid] = not any(alive.get(r, False) for r in retracts.get(eid, []))
    return {eid for eid, ok in alive.items() if ok}


def clip_start_us(filename: str) -> int:
    stamp = re.search(r"(\d{8}T\d{6})\.[a-z0-9]+$", filename)
    if stamp is None:
        msg = f"no start stamp in {filename}"
        raise ValueError(msg)
    at = datetime.strptime(stamp.group(1), "%Y%m%dT%H%M%S").replace(tzinfo=UTC)
    return int(at.timestamp()) * 1_000_000


def namings(lab: Path, clip: int) -> tuple[str, str, list[tuple[float, float, str]]]:
    """The clip's source, filename and in-force namings, seconds into it."""
    ingest = sqlite3.connect(f"file:{lab / 'ingest.sqlite'}?mode=ro", uri=True)
    source, filename = ingest.execute(
        "SELECT source, filename FROM clips WHERE id = ?", (clip,)
    ).fetchone()
    meaning = sqlite3.connect(f"file:{lab / 'recall.sqlite'}?mode=ro", uri=True)
    rows = [
        (eid, json.loads(act))
        for eid, act in meaning.execute("SELECT id, act FROM edits ORDER BY id")
    ]
    alive = in_force(rows)
    start = clip_start_us(filename)
    latest: dict[tuple[int, int], str] = {}
    for eid, act in rows:
        s = act.get("Speaker")
        if eid in alive and s is not None and s["clip"] == clip:
            latest[(s["span"]["start"], s["span"]["end"])] = s["name"]
    spans = [((a - start) / 1e6, (b - start) / 1e6, n) for (a, b), n in latest.items()]
    return source, filename, spans


def production(lab: Path, filename: str) -> list[Turn]:
    ingest = sqlite3.connect(f"file:{lab / 'ingest.sqlite'}?mode=ro", uri=True)
    (raw,) = ingest.execute(
        "SELECT result FROM jobs WHERE kind = 'diarize-segment' AND filename = ?",
        (filename,),
    ).fetchone()
    return [
        (t["speaker"], t["start"], t["end"]) for t in json.loads(raw)["result"]["turns"]
    ]


def longest(turns: list[Turn], a: float, b: float) -> str | None:
    """The voice speaking most of `a..b`, if any."""
    seconds: dict[str, float] = defaultdict(float)
    for speaker, s, e in turns:
        o = min(e, b) - max(s, a)
        if o > 0:
            seconds[speaker] += o
    return max(seconds, key=lambda k: seconds[k]) if seconds else None


def score(turns: list[Turn], spans: list[tuple[float, float, str]]) -> str:
    on: list[tuple[str | None, str, float]] = []
    for a, b, name in spans:
        on.append((longest(turns, a, b), name, b - a))
    names: dict[str | None, dict[str, float]] = defaultdict(lambda: defaultdict(float))
    counts: dict[str | None, Counter[str]] = defaultdict(Counter)
    for voice, name, seconds in on:
        names[voice][name] += seconds
        counts[voice][name] += 1
    # On a voice mostly named as someone else: no print can name it right.
    merged = sum(
        1 for v, n, _ in on if v is not None and counts[v].most_common(1)[0][0] != n
    )
    named = sum(sum(c.values()) for v, c in names.items() if v is not None)
    pure = sum(max(c.values()) for v, c in names.items() if v is not None)
    voices = len({t[0] for t in turns})
    lost = sum(1 for v, _, _ in on if v is None)
    return (
        f"merged {merged:>3}/{len(on):<3} purity {pure / max(named, 1e-9):6.1%} "
        f"voices {voices:>2} (named {len([v for v in names if v is not None])}) "
        f"uncovered {lost}"
    )


def both(audio: Path) -> tuple[list[Turn], list[Turn]]:
    """The default pipeline's exclusive turns (what production stores) and its
    overlapping ones, from one run."""
    import torch  # noqa: PLC0415 - heavy

    from recall.asr import decode_pcm_f32  # noqa: PLC0415 - heavy
    from recall.diarize import _pipeline  # noqa: PLC0415 - heavy

    pipeline = _pipeline("pyannote/speaker-diarization-3.1", None, None, None)
    samples = decode_pcm_f32(audio, sample_rate=16000).copy()
    result = pipeline(
        {"waveform": torch.from_numpy(samples).unsqueeze(0), "sample_rate": 16000}
    )  # type: ignore[operator]  # pyannote is untyped
    exclusive = getattr(result, "exclusive_speaker_diarization", result)
    overlapping = getattr(result, "speaker_diarization", result)

    def turns(annotation: object) -> list[Turn]:
        return [
            (str(s), float(seg.start), float(seg.end))
            for seg, _, s in annotation.itertracks(yield_label=True)  # type: ignore[attr-defined]  # a pyannote Annotation, untyped
        ]

    return turns(exclusive), turns(overlapping)


def overlap_report(
    exclusive: list[Turn],
    overlapping: list[Turn],
    spans: list[tuple[float, float, str]],
) -> str:
    """Of the namings on a voice mostly named as someone else: how many have
    the named person's own voice speaking under the span in the overlapping
    turns, i.e. lost to overlap rather than to clustering."""

    on = [(longest(exclusive, a, b), a, b, n) for a, b, n in spans]
    counts: dict[str | None, Counter[str]] = defaultdict(Counter)
    for v, _, _, n in on:
        counts[v][n] += 1
    home: dict[str, str] = {}
    for v, c in counts.items():
        if v is not None:
            name = c.most_common(1)[0][0]
            if c[name] > counts.get(home.get(name), Counter()).get(name, 0):
                home[name] = v
    minority = [
        (a, b, n)
        for v, a, b, n in on
        if v is not None and counts[v].most_common(1)[0][0] != n
    ]
    heard = 0
    no_home = 0
    for a, b, n in minority:
        if n not in home:
            no_home += 1
            continue
        if any(s == home[n] and min(y, b) - max(x, a) > 0 for s, x, y in overlapping):
            heard += 1
    seconds = sorted(b - a for a, b, _ in minority)
    mid = seconds[len(seconds) // 2] if seconds else 0.0
    return (
        f"misplaced {len(minority)}/{len(spans)}: own voice speaking under it in the "
        f"overlapping turns {heard}, no voice of their own {no_home}; "
        f"median length {mid:.1f} s"
    )


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("lab", type=Path)
    ap.add_argument("clips", type=int, nargs="+")
    ap.add_argument("--thresholds", type=float, nargs="*", default=[])
    ap.add_argument("--overlap", action="store_true", help="the overlap test instead")
    args = ap.parse_args()
    from recall.diarize import pyannote_diarize  # noqa: PLC0415 - heavy

    if args.overlap:
        for clip in args.clips:
            source, filename, spans = namings(args.lab, clip)
            exclusive, overlapping = both(args.lab / "ingest" / source / filename)
            print(
                f"clip {clip}: {overlap_report(exclusive, overlapping, spans)}",
                flush=True,
            )
        return 0

    for clip in args.clips:
        source, filename, spans = namings(args.lab, clip)
        print(f"clip {clip} ({len(spans)} namings)", flush=True)
        print(
            f"  production    {score(production(args.lab, filename), spans)}",
            flush=True,
        )
        audio = args.lab / "ingest" / source / filename
        for threshold in [None, *args.thresholds]:
            turns = pyannote_diarize(audio, clustering_threshold=threshold)
            got = [(t.speaker, t.start, t.end) for t in turns]
            label = "default" if threshold is None else f"t={threshold:.3f}"
            print(f"  {label:<13} {score(got, spans)}", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
