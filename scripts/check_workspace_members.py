#!/usr/bin/env python3
"""Every Rust workspace member must reach the places that BUILD it.

⚠ This exists because of a real outage of the deploy path, not a style rule.
On 2026-09-08 the `doctor` crate was added to `Cargo.toml` and to `flake.nix`
but not to the Dockerfile. `cargo build --locked -p recalld` inside the image
then fails — cargo cannot load the workspace graph unless every MEMBER is
present — and it reports that as a bare "No such file or directory" naming no
file at all. Four consecutive image builds failed before anyone looked.

The gate ran 31 checks that day and every one passed, because none of them
builds the Docker image. That is the gap: fleet images are `:latest` only, so a
rollback IS a roll-forward, and a change that breaks the image build makes the
whole fleet undeployable — including in an emergency — while looking green
locally.

⚠ Deliberately a TEXT comparison, not a build. Building the image in the gate
would add minutes to every commit on a machine that is also recording audio,
and the gate's cost already shapes whether people run it. Reading three files
catches the entire failure class for nothing.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def workspace_members() -> set[str]:
    """The `members` list — the set cargo needs present to load the graph."""
    text = (ROOT / "Cargo.toml").read_text()
    block = re.search(r"members\s*=\s*\[(.*?)\]", text, re.S)
    if block is None:
        raise SystemExit("Cargo.toml: no [workspace] members list")
    return set(re.findall(r'"([^"]+)"', block.group(1)))


def dockerfile_copies() -> set[str]:
    """Crate directories the image copies in before it builds."""
    text = (ROOT / "Dockerfile").read_text()
    return set(re.findall(r"^COPY\s+([a-z_][a-z0-9_-]*)/\s", text, re.M))


def flake_paths() -> set[str]:
    """Crate directories the Nix fileset carries into the sandbox."""
    text = (ROOT / "flake.nix").read_text()
    return set(re.findall(r"^\s*\./([a-z_][a-z0-9_-]*)\s*$", text, re.M))


def main() -> int:
    members = workspace_members()
    problems: list[str] = []

    sources = (("Dockerfile", dockerfile_copies()), ("flake.nix", flake_paths()))
    for where, present in sources:
        missing = members - present
        if missing:
            problems.append(
                f"{where} is missing workspace member(s): "
                f"{', '.join(sorted(missing))}\n"
                "  cargo cannot load the graph without them, and the error"
                " will name no file."
            )

    if problems:
        print("\n".join(problems), file=sys.stderr)
        print(
            f"\n[workspace] members = {sorted(members)}",
            file=sys.stderr,
        )
        return 1

    print(
        f"every workspace member ({len(members)}) reaches the Dockerfile and flake.nix"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
