#!/usr/bin/env python3
"""`transcript` stays pure: no dependency that can do IO, no IO in its source.

The crate holds recall's domain and, later, `render` (#1911). Purity there is
the point: a function of its arguments can be tested without a database and
cannot quietly depend on one. Rust has no effect types to say so, so this says
it twice:

- dependencies: an ALLOWLIST of the whole graph, so a new crate is a decision
  made here, never an accident (a ban list would let the next IO crate in);
- source: no `std::fs`, `std::net`, `std::process`, `std::env`, threads, or a
  clock (`SystemTime`, `Utc::now`, `Local::now`, `Instant::now`).
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CRATE = "transcript"

# Each entry is pure computation: dates, serialisation, and the proc-macro
# stack serde's derive builds with.
ALLOWED = {
    "autocfg",
    "chrono",
    "num-traits",
    "proc-macro2",
    "quote",
    "serde",
    "serde_core",
    "serde_derive",
    "syn",
    "transcript",
    "unicode-ident",
}

FORBIDDEN_SOURCE = re.compile(
    r"\bstd::(fs|net|process|env|thread)\b|\bSystemTime\b|\b(Utc|Local)::now\b|\bInstant::now\b"
)


def dependencies() -> set[str]:
    out = subprocess.run(
        [
            "cargo",
            "tree",
            "-p",
            CRATE,
            "-e",
            "normal,build",
            "--prefix",
            "none",
            "--format",
            "{p}",
        ],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return {line.split()[0] for line in out.splitlines() if line.strip()}


def main() -> int:
    problems: list[str] = []
    extra = sorted(dependencies() - ALLOWED)
    if extra:
        problems.append(
            f"{CRATE} depends on {', '.join(extra)}: not on the allowlist in "
            f"scripts/check_pure_crate.py. Add one only if it cannot do IO."
        )
    for path in sorted((ROOT / CRATE).rglob("*.rs")):
        for number, line in enumerate(path.read_text().splitlines(), 1):
            if FORBIDDEN_SOURCE.search(line) and not line.lstrip().startswith("//"):
                where = f"{path.relative_to(ROOT)}:{number}"
                problems.append(
                    f"{where}: IO or a clock in a pure crate: {line.strip()}"
                )
    for problem in problems:
        print(problem, file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
