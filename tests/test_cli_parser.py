"""CLI argument wiring that matters operationally (launchd agents depend on it)."""

from __future__ import annotations

from recall.cli_parser import build_parser


def test_record_accepts_a_device_pin() -> None:
    args = build_parser().parse_args(["record", "--device", "USB Condenser Microphone"])
    assert args.device == "USB Condenser Microphone"


def test_record_device_defaults_to_the_system_default() -> None:
    assert build_parser().parse_args(["record"]).device == ""


def test_live_accepts_a_device_pin() -> None:
    args = build_parser().parse_args(["live", "--device", "USB Condenser Microphone"])
    assert args.device == "USB Condenser Microphone"


def test_live_device_defaults_to_the_system_default() -> None:
    assert build_parser().parse_args(["live"]).device == ""
