"""CLI argument wiring that matters operationally (launchd agents depend on it)."""

from __future__ import annotations

import argparse
from pathlib import Path

import pytest

from recall.cli_parser import build_parser
from recall.paths import FLEET_DATA_ROOT, MAC_DATA_ROOT, default_data_root


def test_live_accepts_a_device_pin() -> None:
    args = build_parser().parse_args(["live", "--device", "USB Condenser Microphone"])
    assert args.device == "USB Condenser Microphone"


def test_live_device_defaults_to_the_system_default() -> None:
    assert build_parser().parse_args(["live"]).device == ""


def test_out_defaults_to_the_mac_archive_not_a_relative_dir(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # A bare `recall transcript` (or doctor) once defaulted --out to ./data — a path on
    # no machine, so it silently opened an empty db in the cwd and answered about
    # nothing. The default must be the archive the machine actually keeps.
    monkeypatch.delenv("RECALL_ROLE", raising=False)
    monkeypatch.delenv("RECALL_OUT", raising=False)
    args = build_parser().parse_args(["transcript", "--day", "today"])
    assert args.out == MAC_DATA_ROOT
    assert args.out != Path("data")


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
