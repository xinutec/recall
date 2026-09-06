"""The model-shim protocol (stage E2).

The protocol half is pure, so it is tested directly; the stdout-isolation half
is tested through a real subprocess, because that is the only place the hazard
it guards against can actually happen.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

from recall.shim import Handler, JsonDict, JsonValue, fail, ok, parse_request, responses


def as_dict(value: JsonValue) -> JsonDict:
    assert isinstance(value, dict), value
    return value


def text(message: JsonDict, key: str) -> str:
    value = message[key]
    assert isinstance(value, str), value
    return value


def run(lines: list[str], handler: Handler | None = None) -> list[JsonDict]:
    def echo(op: str, args: JsonDict) -> JsonValue:
        return {"op": op, **args}

    out = responses(iter(lines), handler or echo, name="test")
    return [json.loads(line) for line in out]


def test_a_request_becomes_a_response_carrying_its_id() -> None:
    [got] = run(['{"id": "7", "op": "transcribe", "audio": "/a.wav"}'])
    assert got["id"] == "7"
    assert got["ok"] is True
    assert got["result"] == {"op": "transcribe", "audio": "/a.wav"}


def test_hello_is_answered_without_the_handler() -> None:
    # The runner uses hello to learn a shim is alive — it must work even for a
    # shim whose model failed to load, so it never reaches the handler.
    def explode(op: str, args: JsonDict) -> JsonValue:
        raise AssertionError("handler must not be called for hello")

    [got] = run(['{"id": "1", "op": "hello"}'], explode)
    assert got["ok"] is True
    assert as_dict(got["result"])["shim"] == "test"


def test_a_malformed_line_is_answered_and_the_loop_continues() -> None:
    # A shim that dies on one bad request loses weights that took seconds to
    # load, and strands the queue behind it.
    got = run(["not json at all", '{"id": "2", "op": "x"}'])
    assert got[0]["ok"] is False
    assert "unparsable" in text(got[0], "error")
    assert got[1]["ok"] is True


def test_a_handler_failure_is_a_response_not_a_crash() -> None:
    def boom(op: str, args: JsonDict) -> JsonValue:
        raise RuntimeError("model exploded")

    [got] = run(['{"id": "3", "op": "transcribe"}'], boom)
    assert got["ok"] is False
    assert got["error"] == "RuntimeError: model exploded"


def test_blank_lines_are_ignored_not_answered() -> None:
    assert run(["", "  ", '{"id": "4", "op": "x"}']) == [
        {"id": "4", "ok": True, "result": {"op": "x"}}
    ]


def test_ok_and_fail_round_trip_unicode_unescaped() -> None:
    # Transcripts are Dutch as often as English; \u escaping would bloat every
    # response and make the wire unreadable by hand.
    assert "café" in ok("1", {"text": "café"})
    assert "café" in fail("1", "café")


def test_a_missing_op_is_unusable_rather_than_guessed() -> None:
    assert parse_request('{"id": "1"}')[1] is None
    assert parse_request("[1, 2, 3]")[1] is None


def test_stdout_pollution_by_the_model_cannot_corrupt_the_wire() -> None:
    # ⚠ THE hazard: mlx-whisper and friends print progress. A stray line would
    # desync the stream silently, because the next parse fails on data that
    # looks almost right. serve() must make library prints go to stderr.
    script = (
        "import sys; sys.path.insert(0, 'src')\n"
        "from recall.shim import serve\n"
        "def h(op, args):\n"
        "    print('progress: 50%')\n"
        "    sys.stdout.write('more noise\\n')\n"
        "    return {'done': True}\n"
        "serve(h, name='noisy')\n"
    )
    proc = subprocess.run(
        [sys.executable, "-c", script],
        input='{"id": "1", "op": "work"}\n',
        capture_output=True,
        text=True,
        cwd=Path(__file__).resolve().parent.parent,
        check=True,
    )
    # Every stdout line must be protocol, and nothing else.
    lines = [ln for ln in proc.stdout.splitlines() if ln.strip()]
    assert len(lines) == 1, f"stdout carried non-protocol lines: {lines}"
    assert json.loads(lines[0]) == {"id": "1", "ok": True, "result": {"done": True}}
    assert "progress: 50%" in proc.stderr
