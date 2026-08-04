#!/usr/bin/env python3
"""Nothing reachable from `recall.api` may import an ML package.

The Isis container has no mlx, no torch, no pyannote — the Mac keeps all of that.
An ML import reachable from `recall.api` CrashLoopBackOffs the fleet pod, so this
poisons every ML module name and then imports the API surface: any module along
the chain that reaches for one fails here instead of on the cluster.

This catches ML creep, and ONLY that. It cannot catch a *missing non-ML* dep — it
runs on the fully-stocked `.venv`, where numpy is present whether the image has it
or not — which is precisely the bug that took Isis down: a lint fix moved
`recall.calibrate` to a top-level import, dragging numpy into `api.py`'s chain,
and the build stayed green all the way to the deploy. The gate for THAT is booting
the real image, which CI does before publishing it
(`.github/workflows/build.yml`). Two different failures, two different checks;
neither substitutes for the other.

Runs on the `.venv` interpreter — the one that has fastapi and pydantic — because
the point is to import the real thing. Was a heredoc inside scripts/verify.sh,
which meant ruff and mypy never saw a line of it.
"""

from __future__ import annotations

import importlib
import sys

#: What the fleet image does NOT have. Set to None in sys.modules so that an
#: `import mlx` anywhere in the chain is a hard failure rather than a fallback.
ML = (
    "mlx",
    "mlx_lm",
    "mlx_whisper",
    "torch",
    "pyannote",
    "faster_whisper",
    "soundfile",
)

#: The entry points the fleet pod actually imports.
FLEET = ("recall.api", "recall.sync")


def main() -> int:
    for name in ML:
        sys.modules[name] = None  # type: ignore[assignment]

    for name in FLEET:
        importlib.import_module(name)

    print(f"no ML reachable from {', '.join(FLEET)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
