"""CLI argument wiring that matters operationally (launchd agents depend on it)."""

from __future__ import annotations

import argparse
from pathlib import Path

import pytest

from recall.cli_parser import build_parser
from recall.paths import FLEET_DATA_ROOT, MAC_DATA_ROOT, default_data_root


def _subparsers() -> dict[str, argparse.ArgumentParser]:
    """Every subcommand, by name, as the parser holds them.

    ⚠ argparse exposes no public way to walk its subcommands, so this reaches for
    the one private attribute that has been stable across every 3.x: the
    `_SubParsersAction` among the parser's actions, and its `choices`. The
    alternative was parsing `--help`, which is a contract nobody promised either
    and is harder to read when it breaks.
    """
    found: dict[str, argparse.ArgumentParser] = {}
    for action in build_parser()._actions:
        if isinstance(action, argparse._SubParsersAction):
            found.update(action.choices)
    return found


def test_every_out_defaults_to_the_mac_archive_not_a_relative_dir(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # A bare `recall <cmd>` once defaulted --out to ./data — a path on no machine, so
    # it silently opened an empty db in the cwd and answered about nothing. The default
    # must be the archive the machine actually keeps.
    #
    # ⚠ Checked over EVERY subcommand rather than one chosen as a specimen. This test
    # named `transcript` until 2026-09-15, when that subcommand was deleted and took
    # the whole rule's coverage with it — a rule this broad should not rest on one
    # command outliving the others.
    monkeypatch.delenv("RECALL_ROLE", raising=False)
    monkeypatch.delenv("RECALL_OUT", raising=False)
    checked = 0
    for name, parser in _subparsers().items():
        defaults = {a.dest: a.default for a in parser._actions if a.dest == "out"}
        if "out" not in defaults:
            continue
        checked += 1
        assert defaults["out"] == MAC_DATA_ROOT, f"{name} --out"
        assert defaults["out"] != Path("data"), f"{name} --out"
    assert checked > 5, f"only {checked} subcommands take --out; the parser moved"


def test_default_data_root_follows_the_role() -> None:
    # Same code on both machines; the archive lives in different places. The fleet node
    # serves /data from its PVC, the Mac holds the master archive on its external disk.
    assert MAC_DATA_ROOT != FLEET_DATA_ROOT


def test_default_data_root_honours_an_explicit_env(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # `recall api` exports RECALL_OUT from its --out; it must win over the role default.
    monkeypatch.setenv("RECALL_ROLE", "fleet")
    monkeypatch.setenv("RECALL_OUT", "/somewhere/else")
    assert default_data_root() == Path("/somewhere/else")


def test_default_data_root_is_fleet_pvc_on_the_record_node(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("RECALL_OUT", raising=False)
    monkeypatch.setenv("RECALL_ROLE", "fleet")
    assert default_data_root() == FLEET_DATA_ROOT


def _registered_subcommands(parser: argparse.ArgumentParser) -> set[str]:
    """Every subcommand name the parser accepts.

    argparse exposes no public accessor for this, so the sub-parsers action is
    found by type among the parser's actions — narrow and typed, rather than
    reaching down a chain of private attributes that mypy cannot check.
    """
    for action in parser._actions:
        if isinstance(action, argparse._SubParsersAction):
            return set(action.choices)
    raise AssertionError("the parser registers no subcommands")


def test_every_subcommand_has_a_handler_and_every_handler_a_subcommand() -> None:
    """The two halves of the CLI must not drift apart.

    ⚠ This was written after shipping three of them. Cutting Ask, summaries and
    the quiet review removed their entries from `_COMMANDS` but left their
    subparsers behind, so `recall summarize` was still advertised in --help and
    crashed with a bare `KeyError: 'summarize'` traceback. Neither ruff nor mypy
    can see it: the parser is built at runtime and the dispatch is a dict lookup,
    so nothing statically connects them.

    A handler with no subparser is the same fault mirrored — unreachable code
    that reads as live.
    """
    from recall.cli import _COMMANDS  # noqa: PLC0415 - defers the ML stack import

    subcommands = _registered_subcommands(build_parser())
    handlers = set(_COMMANDS)

    assert subcommands - handlers == set(), "advertised in --help, crashes when run"
    assert handlers - subcommands == set(), "reachable by no command line"
