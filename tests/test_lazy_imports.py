"""Every `recall.*` import in the package resolves — including the lazy ones.

A lazy import (inside a function, behind `# noqa: PLC0415`) is not loaded until
its code runs, so deleting the module it names leaves ruff, mypy and the suite
green while that code fails at runtime with `ImportError`.

⚠ Checked against a broken import: point one lazy import at a missing module and
this fails, naming the file, the module and the line.

⚠ `mypy` is no substitute: its incremental cache can still hold a module that was
just deleted, which is exactly when this matters.
"""

from __future__ import annotations

import ast
import importlib
import pathlib

import pytest

_SRC = pathlib.Path(__file__).resolve().parents[1] / "src" / "recall"


def _recall_imports() -> list[tuple[str, str, int]]:
    """Every `from recall.x import ...` / `import recall.x` in the package."""
    found: list[tuple[str, str, int]] = []
    for path in sorted(_SRC.glob("*.py")):
        tree = ast.parse(path.read_text())
        for node in ast.walk(tree):
            if isinstance(node, ast.ImportFrom) and node.module:
                if node.module.startswith("recall."):
                    found.append((path.name, node.module, node.lineno))
            elif isinstance(node, ast.Import):
                found.extend(
                    (path.name, alias.name, node.lineno)
                    for alias in node.names
                    if alias.name.startswith("recall.")
                )
    return found


@pytest.mark.parametrize(
    ("source", "module", "line"),
    _recall_imports(),
    ids=str,
)
def test_every_recall_import_resolves(source: str, module: str, line: int) -> None:
    # ⚠ The heavy modules (mlx, torch, pyannote) are imported lazily ON purpose so
    # the capture agents stay ML-free. Importing them here is fine — the test
    # environment has them — and it is the only way to prove the name is real.
    try:
        importlib.import_module(module)
    except ImportError as exc:  # pragma: no cover - the failure is the point
        pytest.fail(f"{source}:{line} imports {module}, which does not resolve: {exc}")
