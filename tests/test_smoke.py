"""Baseline test: the package imports and exposes a version.

Exists so Phase 0 starts from a green, fully-typed baseline (TDD-first).
"""

from __future__ import annotations

import recall


def test_version_is_exposed() -> None:
    assert recall.__version__ == "0.0.0"
