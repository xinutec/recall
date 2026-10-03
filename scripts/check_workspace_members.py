#!/usr/bin/env python3
"""Every Rust workspace member must reach the places that BUILD it.

A member missing from the Dockerfile fails the image build with a bare "No such
file or directory", and the gate builds no image, so the break shows only in CI.
Fleet images are `:latest` only, so a broken image build leaves the fleet
undeployable, in an emergency too, while every local check is green.

⚠ A text comparison, not a build: building the image would add minutes to every
commit, and reading three files catches the whole failure class.
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
        # `experimental/playback` arrives with its top directory.
        missing = {m for m in members if m.split("/")[0] not in present}
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
