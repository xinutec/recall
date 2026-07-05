#!/usr/bin/env python3
"""Focused mutation tester — are the tests actually catching bugs?

Mutates one module a single node at a time (flip a comparison, swap +/-, bump a
constant, negate a bool), runs that module's tests, and reports every mutant the
tests FAILED to kill. A survivor is a behaviour the suite doesn't pin down — i.e.
a missing test (or, occasionally, an equivalent mutant to eyeball).

Pure stdlib (ast + subprocess); no mutmut dependency, so it travels with the repo
and stays under our control like dev-lint and gen_models.

  scripts/mutants.py src/recall/ranking.py \
      --test '.venv/bin/python -m pytest tests/test_ranking.py -q'
"""

from __future__ import annotations

import argparse
import ast
import os
import shlex
import subprocess
from collections.abc import Callable, Iterator
from dataclasses import dataclass
from pathlib import Path

# Operator swaps: each produces a *different* behaviour, so a good test must notice.
_CMP = {
    ast.Lt: ast.LtE,
    ast.LtE: ast.Lt,
    ast.Gt: ast.GtE,
    ast.GtE: ast.Gt,
    ast.Eq: ast.NotEq,
    ast.NotEq: ast.Eq,
}
_BIN = {ast.Add: ast.Sub, ast.Sub: ast.Add, ast.Mult: ast.Div, ast.Div: ast.Mult}
_BOOL = {ast.And: ast.Or, ast.Or: ast.And}

_Apply = Callable[[ast.AST], None]


@dataclass(frozen=True)
class Mutant:
    lineno: int
    desc: str
    source: str


# Factories so each apply captures its operator/value through a typed parameter (a
# bare lambda with a default-arg closure can't be type-inferred against `_Apply`).
def _cmp_apply(op: type[ast.cmpop]) -> _Apply:
    return lambda target: _set_cmp(target, op)


def _op_apply(op: type[ast.operator] | type[ast.boolop]) -> _Apply:
    return lambda target: _set_op(target, op)


def _const_apply(value: bool | int | float) -> _Apply:
    return lambda target: _set_const(target, value)


def _swaps(node: ast.AST) -> Iterator[tuple[str, _Apply]]:
    """(description, apply) for each single-node mutation `node` admits."""
    if isinstance(node, ast.Compare) and len(node.ops) == 1:
        cur = type(node.ops[0])
        nxt = _CMP.get(cur)
        if nxt is not None:
            yield f"{cur.__name__}->{nxt.__name__}", _cmp_apply(nxt)
    elif isinstance(node, ast.BinOp) and type(node.op) in _BIN:
        bin_op = _BIN[type(node.op)]
        yield f"{type(node.op).__name__}->{bin_op.__name__}", _op_apply(bin_op)
    elif isinstance(node, ast.BoolOp):
        bool_op = _BOOL[type(node.op)]
        yield f"{type(node.op).__name__}->{bool_op.__name__}", _op_apply(bool_op)
    elif isinstance(node, ast.Constant):
        yield from _const_swaps(node.value)


def _const_swaps(value: object) -> Iterator[tuple[str, _Apply]]:
    if isinstance(value, bool):
        yield f"{value}->{not value}", _const_apply(not value)
    elif isinstance(value, int):
        yield f"{value}->{value + 1}", _const_apply(value + 1)
    elif isinstance(value, float):
        yield f"{value}->{value + 1.0}", _const_apply(value + 1.0)


def _set_cmp(target: ast.AST, op: type[ast.cmpop]) -> None:
    assert isinstance(target, ast.Compare)
    target.ops[0] = op()


def _set_op(target: ast.AST, op: type[ast.operator] | type[ast.boolop]) -> None:
    assert isinstance(target, ast.BinOp | ast.BoolOp)
    target.op = op()


def _set_const(target: ast.AST, value: bool | int | float) -> None:
    assert isinstance(target, ast.Constant)
    target.value = value


def _nth(tree: ast.Module, index: int) -> ast.AST:
    return list(ast.walk(tree))[index]


def mutants(source: str) -> list[Mutant]:
    count = sum(1 for _ in ast.walk(ast.parse(source)))
    out: list[Mutant] = []
    for index in range(count):
        node = _nth(ast.parse(source), index)
        for desc, apply in _swaps(node):
            tree = ast.parse(source)
            apply(_nth(tree, index))
            lineno = getattr(node, "lineno", 0)
            out.append(Mutant(lineno, desc, ast.unparse(tree)))
    return out


# Same-size mutations (e.g. `*`->`/`, `0.6`->`1.6`) can land on the same mtime as
# the previous mutant, so a cached .pyc would mask the change. Forbid bytecode so
# every run compiles the mutated source afresh.
_NO_BYTECODE = {**os.environ, "PYTHONDONTWRITEBYTECODE": "1"}


def _passes(cmd: list[str]) -> bool:
    return (
        subprocess.run(
            cmd, capture_output=True, check=False, env=_NO_BYTECODE
        ).returncode
        == 0
    )


def _clear_pyc(module: Path) -> None:
    """Drop any pre-existing bytecode for the module under test."""
    cache = module.parent / "__pycache__"
    for pyc in cache.glob(f"{module.stem}.*.pyc"):
        pyc.unlink()


def main() -> int:
    parser = argparse.ArgumentParser(description="Focused mutation tester.")
    parser.add_argument("module", type=Path)
    parser.add_argument("--test", required=True, help="test command to run per mutant")
    args = parser.parse_args()
    module: Path = args.module
    cmd = shlex.split(args.test)

    original = module.read_bytes()
    population = mutants(original.decode())
    _clear_pyc(module)
    if not _passes(cmd):
        raise SystemExit("tests fail on the unmutated module — fix that first")

    survivors: list[Mutant] = []
    try:
        for mutant in population:
            module.write_text(mutant.source, encoding="utf-8")
            if _passes(cmd):  # the suite didn't notice the change
                survivors.append(mutant)
    finally:
        module.write_bytes(original)

    killed = len(population) - len(survivors)
    print(f"{module}: {killed}/{len(population)} mutants killed")
    for mutant in survivors:
        print(f"  SURVIVED  {module}:{mutant.lineno}  {mutant.desc}")
    return 1 if survivors else 0


if __name__ == "__main__":
    raise SystemExit(main())
