"""The recall-api CLI — loaded by path, since it's a standalone dependency-free script.

Tests the public entry (`main`) with the HTTP call stubbed: argument routing to the real
API paths and the markdown rendering, without needing a running server.
"""

from __future__ import annotations

import importlib.util
from pathlib import Path
from types import ModuleType

import pytest


def _load() -> ModuleType:
    path = Path(__file__).resolve().parent.parent / "scripts" / "recall-api.py"
    spec = importlib.util.spec_from_file_location("recall_api_cli", path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


_cli = _load()


def test_main_routes_a_command_to_its_api_path(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    seen: dict[str, object] = {}

    def fake_fetch(base: str, path: str, params: dict[str, object]) -> object:
        seen["base"], seen["path"], seen["params"] = base, path, params
        return {"items": []}

    monkeypatch.setattr(_cli, "fetch", fake_fetch)
    assert _cli.main(["search", "birthday", "--limit", "20"]) == 0
    assert seen["path"] == "/api/search"
    assert seen["params"] == {"q": "birthday", "limit": 20}
    assert '"items": []' in capsys.readouterr().out


def test_main_renders_transcript_markdown(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    payload = {
        "turns": [
            {"start": "2026-01-15T10:41:51+01:00", "speaker": "Pippijn", "text": "Hi."},
            {"start": "2026-01-15T10:42:03+01:00", "speaker": "Dr L", "text": "Bye."},
        ]
    }
    monkeypatch.setattr(_cli, "fetch", lambda *_a, **_k: payload)
    assert _cli.main(["transcript", "meeting-x", "--markdown"]) == 0
    assert capsys.readouterr().out == (
        "**[10:41] Pippijn:** Hi.\n\n**[10:42] Dr L:** Bye.\n"
    )
