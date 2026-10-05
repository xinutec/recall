"""Every `recall.*` import in the package resolves, including the lazy ones.

A lazy import (inside a function) is not loaded until its code runs, so ruff,
mypy (whose cache can still hold a deleted module) and the rest of the suite
stay green when its module is gone.
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
    # The test venv has the heavy modules, so importing them proves the name.
    try:
        importlib.import_module(module)
    except ImportError as exc:  # pragma: no cover - the failure is the point
        pytest.fail(f"{source}:{line} imports {module}, which does not resolve: {exc}")
