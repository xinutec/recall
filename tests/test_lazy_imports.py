"""Every `recall.*` import in the package resolves — including the lazy ones.

⚠ **The gap this closes, found the hard way on 2026-09-14.** `cli.py` imports
several handlers' modules INSIDE the handler, behind a `# noqa: PLC0415`, so the
module is not loaded until someone runs that subcommand. Delete the module and
ruff, mypy and the whole test suite stay green while the subcommand dies on an
`ImportError` at runtime — which is exactly what happened to `capture-mirror`
when its Python was deleted and its handler was not.

⚠ **Ablated, so this is not a vacuous green.** Point one lazy import at a module
that does not exist and this fails, naming the file, the module and the line.
`ruff` passes over the same tree.

⚠ **`mypy` is not a reliable substitute here.** It caught the ablation — and it
did NOT catch the real `capture-mirror` case hours earlier, reporting "Success:
no issues found" over a `cli.py` that imported a module I had just deleted. The
difference is most likely its incremental cache still holding the removed
module, which is exactly the state you are in right after a deletion. A check
that works except when you need it is why this test exists rather than a note
saying "mypy covers it".
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
    # ⚠ The heavy modules (mlx, torch, pyannote) are imported lazily ON PURPOSE so
    # the capture agents stay ML-free. Importing them here is fine — the test
    # environment has them — and it is the only way to prove the name is real.
    try:
        importlib.import_module(module)
    except ImportError as exc:  # pragma: no cover - the failure is the point
        pytest.fail(f"{source}:{line} imports {module}, which does not resolve: {exc}")
