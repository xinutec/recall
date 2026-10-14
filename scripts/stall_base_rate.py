"""How often a build is active at a random minute, against at an archive stall.

`sample` lists processes the way the doctor does at a stall (`ps -A -o
pid=,state=,%cpu=,comm=`, kept: disk wait or a known bulk writer) once a
minute, at a random second, and appends each listing to OUT as one JSON line.

`compare` classifies those samples and the doctor's stall blocks in the same
window with one rule: a process counts as active in `U` or `R` or above 5%
CPU, the doctor itself excluded.

Usage:
  python scripts/stall_base_rate.py sample OUT --hours 24
  python scripts/stall_base_rate.py compare OUT LOG... [--since ISO] [--until ISO]
  LOG: the doctor's stderr log (doctor.err.log).
"""

from __future__ import annotations

import argparse
import json
import random
import re
import subprocess
import sys
import time
from collections import Counter
from datetime import UTC, datetime
from pathlib import Path

# The doctor's HEAVY_WRITERS (doctor/src/bounded.rs).
HEAVY_WRITERS = {
    "cargo",
    "rustc",
    "ld",
    "ld64",
    "clang",
    "nix",
    "nix-daemon",
    "restic",
    "rsync",
    "git",
    "mds_stores",
    "mdworker_shared",
    "backupd",
}
BUILD = {"cargo", "rustc", "ld", "ld64", "clang", "nix", "nix-daemon"}
CAP = 20
# `pid state %cpu comm`.
PS_FIELDS = 4
# Above this a process counts as busy, as in the first count of stalls.
ACTIVE_CPU = 5.0

Row = tuple[str, float, str]

STALL = re.compile(r"^(\S+Z) doctor: the archive did not answer")
SUSPECT = re.compile(r"^\s+on the disk then: \d+ (\S+) ([\d.]+)% (.+)$")


def suspects(ps_output: str) -> list[Row]:
    """The doctor's `disk_suspects`: disk waits first, then bulk writers."""
    waiting: list[Row] = []
    writers: list[Row] = []
    for line in ps_output.splitlines():
        fields = line.split()
        if len(fields) < PS_FIELDS:
            continue
        state, cpu, name = (
            fields[1],
            float(fields[2]),
            " ".join(fields[3:]).rsplit("/", 1)[-1],
        )
        if state.startswith("U"):
            waiting.append((state, cpu, name))
        elif name in HEAVY_WRITERS:
            writers.append((state, cpu, name))
    return (waiting + writers)[:CAP]


def classify(rows: list[Row]) -> str:
    active = {
        name
        for state, cpu, name in rows
        if name != "doctor" and (state[0] in "UR" or cpu > ACTIVE_CPU)
    }
    if active & BUILD:
        return "build"
    if active == {"node"}:
        return "node only"
    return "other" if active else "nothing"


def sample(out: Path, hours: float) -> None:
    end = time.time() + hours * 3600
    while time.time() < end:
        minute = time.time() // 60 * 60
        time.sleep(max(0.0, minute + random.uniform(0, 60) - time.time()))
        listing = subprocess.run(
            ["ps", "-A", "-o", "pid=,state=,%cpu=,comm="],
            capture_output=True,
            text=True,
            check=True,
        ).stdout
        utc = datetime.now(UTC).strftime("%Y-%m-%dT%H:%M:%SZ")
        with out.open("a") as f:
            f.write(json.dumps({"utc": utc, "rows": suspects(listing)}) + "\n")
        time.sleep(max(0.0, minute + 60 - time.time()))


def stalls(logs: list[Path]) -> dict[str, list[Row]]:
    found: dict[str, list[Row]] = {}
    current: list[Row] | None = None
    for log in logs:
        for line in log.read_text(errors="replace").splitlines():
            if m := STALL.match(line):
                current = found.setdefault(m[1], [])
            elif current is not None and (m := SUSPECT.match(line)):
                current.append((m[1], float(m[2]), m[3]))
            elif not line.startswith(" "):
                current = None
    return found


def table(title: str, classes: Counter[str]) -> None:
    total = sum(classes.values())
    print(f"{title}: {total}")
    for name in ("build", "node only", "other", "nothing"):
        share = f"{100 * classes[name] / total:5.1f}%" if total else "    -"
        print(f"  {name:10} {classes[name]:5} {share}")


def compare(out: Path, logs: list[Path], since: str | None, until: str | None) -> None:
    samples = [json.loads(line) for line in out.read_text().splitlines() if line]
    lo = since or (samples[0]["utc"] if samples else "")
    hi = until or (samples[-1]["utc"] if samples else "~")
    print(f"window {lo} .. {hi}")
    table(
        "random minutes",
        Counter(
            classify([tuple(r) for r in s["rows"]])
            for s in samples
            if lo <= s["utc"] <= hi
        ),
    )
    table(
        "stalls",
        Counter(classify(rows) for at, rows in stalls(logs).items() if lo <= at <= hi),
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="mode", required=True)
    s = sub.add_parser("sample")
    s.add_argument("out", type=Path)
    s.add_argument("--hours", type=float, default=24.0)
    c = sub.add_parser("compare")
    c.add_argument("out", type=Path)
    c.add_argument("logs", type=Path, nargs="+")
    c.add_argument("--since")
    c.add_argument("--until")
    args = parser.parse_args()
    if args.mode == "sample":
        sample(args.out, args.hours)
    else:
        compare(args.out, args.logs, args.since, args.until)
    return 0


if __name__ == "__main__":
    sys.exit(main())
