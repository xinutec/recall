"""Does cropping a clip to its speech before Whisper help? A lab referee.

The fleet transcribes whole clips with the household prompt (names and
vocabulary). One clip showed that combination looping over 16 s of leading
noise, while the same clip cropped to its measured speech came out right. This
measures that on two groups, three arms each:

* looped: clips whose transcript the passes retired as a repetition loop.
  Scored by whether the output still loops, and how many words survive.
* checked: clips holding lines whose words a person checked. Scored by the
  error rate of those words against what each arm heard over the same span.

Arms: whole clip with the prompt (production), the clip cropped to
[first speech - PAD_S, last speech + PAD_S] with the prompt (the change), and
the whole clip without the prompt (to see what the prompt itself costs).

Usage, in the ML env (`nix build .#ml-env`), with ffmpeg on PATH:

    crop_referee.py run   --work DIR   # resumable; writes DIR/results.jsonl
    crop_referee.py score --work DIR

DIR holds, gathered read-only from the fleet: `truth.tsv` (clip path, clip
start, line start, line end, checked text), `truth-regions.tsv` (filename,
regions), `looped.tsv` (outcome, source, filename, regions), `terms.txt` (names,
`---`, vocabulary) and the clips under `clips/`.
"""

from __future__ import annotations

import argparse
import itertools
import json
import random
import re
import sys
import tempfile
from collections import defaultdict
from datetime import datetime
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "src"))
sys.path.insert(0, str(Path(__file__).resolve().parent))

from room_referee import infix_error_rate

PAD_S = 1.0
"""Margin kept either side of the measured speech: VAD edges can clip an onset."""

MAX_PROMPT_CHARS = 600
"""As `recalld::labels::MAX_PROMPT_CHARS`."""

SAMPLE_PER_OUTCOME = 30
LOOP_WORDS = 8
"""One token this many times running is a loop."""
SPAN_SLACK_S = 2.0
"""How far outside a checked line's span an arm's word may sit and still count."""

ARMS = ("whole+prompt", "crop+prompt", "whole")
RESULTS = "results.jsonl"


def prompt_of(terms: str) -> str:
    """The prompt as `recalld::labels::initial_prompt` builds it."""
    names, _, vocabulary = terms.partition("---\n")
    prompt = ""
    seen: set[str] = set()
    for term in [t for t in names.splitlines() + vocabulary.splitlines() if t]:
        if term in seen:
            continue
        seen.add(term)
        extended = term if not prompt else f"{prompt}, {term}"
        if len(extended) > MAX_PROMPT_CHARS:
            break
        prompt = extended
    return prompt


def crop_window(regions: list[list[float]]) -> tuple[float, float] | None:
    """The stretch to transcribe, or None for a clip with no speech."""
    if not regions:
        return None
    return max(0.0, regions[0][0] - PAD_S), regions[-1][1] + PAD_S


def _seconds(a: str, b: str) -> float:
    return (datetime.fromisoformat(b) - datetime.fromisoformat(a)).total_seconds()


def cases(work: Path) -> list[dict[str, Any]]:
    """Every clip to run: the checked ones whole, the looped ones sampled."""
    regions = dict(
        line.split("\t", 1)
        for line in (work / "truth-regions.tsv").read_text().splitlines()
    )
    checked: dict[str, dict[str, Any]] = {}
    for line in (work / "truth.tsv").read_text().splitlines():
        path, clip_start, start, end, text = line.split("\t", 4)
        name = Path(path).name
        if name not in regions:
            continue
        case = checked.setdefault(
            name,
            {
                "group": "checked",
                "clip": f"{Path(path).parent.name}/{name}",
                "regions": json.loads(regions[name]),
                "truth": [],
            },
        )
        case["truth"].append(
            {
                "start": _seconds(clip_start, start),
                "end": _seconds(clip_start, end),
                "text": text,
            }
        )
    looped: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for line in (work / "looped.tsv").read_text().splitlines():
        outcome, source, name, raw = line.split("\t", 3)
        looped[outcome].append(
            {
                "group": outcome,
                "clip": f"{source}/{name}",
                "regions": json.loads(raw),
                "truth": [],
            }
        )
    rng = random.Random(1764)
    sampled = [
        case
        for outcome in sorted(looped)
        for case in rng.sample(
            looped[outcome], min(SAMPLE_PER_OUTCOME, len(looped[outcome]))
        )
    ]
    return sorted(checked.values(), key=lambda c: c["clip"]) + sampled


def transcribe(
    clip: Path, arm: str, window: tuple[float, float] | None, prompt: str
) -> list[dict[str, Any]]:
    """The arm's words, in seconds from the clip's start."""
    from recall.asr import mlx_transcribe, slice_clip  # noqa: PLC0415 - heavy

    offset = 0.0
    with tempfile.TemporaryDirectory() as scratch:
        audio = clip
        if arm == "crop+prompt" and window is not None:
            offset = window[0]
            audio = Path(scratch) / "crop.wav"
            slice_clip(clip, audio, window[0], window[1])
        result = mlx_transcribe(
            audio, words=True, initial_prompt=prompt if arm.endswith("prompt") else None
        )
    return [
        {"start": w.start + offset, "end": w.end + offset, "text": w.text}
        for s in result.segments
        for w in s.words
    ]


def run(work: Path, results: str) -> None:
    prompt = prompt_of((work / "terms.txt").read_text())
    out = work / results
    done = set()
    if out.exists():
        done = {
            (r["clip"], r["arm"]) for r in map(json.loads, out.read_text().splitlines())
        }
    todo = cases(work)
    print(f"{len(todo)} clips, {len(done)} arm runs already done", flush=True)
    with out.open("a") as sink:
        for n, case in enumerate(todo, 1):
            clip = work / "clips" / case["clip"]
            if not clip.is_file():
                continue
            for arm in ARMS:
                if (case["clip"], arm) in done:
                    continue
                words = transcribe(clip, arm, crop_window(case["regions"]), prompt)
                sink.write(json.dumps({**case, "arm": arm, "words": words}) + "\n")
                sink.flush()
            if n % 10 == 0:
                print(f"{n} clips", flush=True)
    print("ALL-DONE", flush=True)


def loops(words: list[dict[str, Any]]) -> bool:
    """A repetition loop, roughly as `audiocore::text::is_repetition_loop` has it:
    one token eight times running, or one character twenty times running."""
    text = "".join(w["text"] for w in words)
    if re.search(r"(.)\1{19}", text):
        return True
    tokens = re.findall(r"\w+", text.lower())
    run = 1
    for a, b in itertools.pairwise(tokens):
        run = run + 1 if a == b else 1
        if run >= LOOP_WORDS:
            return True
    return False


def heard_over(words: list[dict[str, Any]], start: float, end: float) -> str:
    return " ".join(
        w["text"].strip()
        for w in words
        if start - SPAN_SLACK_S <= (w["start"] + w["end"]) / 2 <= end + SPAN_SLACK_S
    )


def score(work: Path, results: str) -> None:
    rows = [json.loads(line) for line in (work / results).read_text().splitlines()]
    by_group: dict[str, dict[str, list[dict[str, Any]]]] = defaultdict(
        lambda: defaultdict(list)
    )
    for row in rows:
        by_group[row["group"]][row["arm"]].append(row)
    for group, arms in sorted(by_group.items()):
        print(f"== {group}")
        for arm in ARMS:
            got = arms.get(arm, [])
            if not got:
                continue
            looped = sum(loops(r["words"]) for r in got)
            line = f"  {arm:13} clips {len(got):3}  looping {looped:3}"
            if group == "checked":
                rates = sorted(
                    infix_error_rate(
                        t["text"], heard_over(r["words"], t["start"], t["end"])
                    )
                    for r in got
                    for t in r["truth"]
                )
                words = sum(len(t["text"].split()) for r in got for t in r["truth"])
                errors = sum(
                    infix_error_rate(
                        t["text"], heard_over(r["words"], t["start"], t["end"])
                    )
                    * len(t["text"].split())
                    for r in got
                    for t in r["truth"]
                )
                perfect = sum(r == 0 for r in rates)
                line += (
                    f"  lines {len(rates)}  word error {errors / words:.1%}"
                    f"  median {rates[len(rates) // 2]:.2f}"
                    f"  exact {perfect}/{len(rates)}"
                )
            else:
                kept = sum(len(r["words"]) for r in got if not loops(r["words"]))
                line += f"  words in non-looping output {kept}"
            print(line)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("command", choices=("run", "score", "list"))
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument(
        "--results", default=RESULTS, help="file under --work, to keep runs apart"
    )
    args = parser.parse_args()
    if args.command == "list":
        for case in cases(args.work):
            print(case["clip"])
    elif args.command == "run":
        run(args.work, args.results)
    else:
        score(args.work, args.results)
    return 0


if __name__ == "__main__":
    sys.exit(main())
