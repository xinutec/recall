"""The client end of the model holder: it really speaks HTTP, and it fails loudly.

A holder that cannot be reached must NOT quietly fall back to loading the weights
in this process — that is the second copy the holder exists to prevent. These
tests run against a real socket, so the failure paths are the real ones.
"""

from __future__ import annotations

import json
import threading
from collections.abc import Iterator
from http.server import BaseHTTPRequestHandler, HTTPServer
from typing import Any, ClassVar, override

import pytest

from recall.llm import (
    DEFAULT_LLM,
    ChatModel,
    LlmHostUnavailable,
    generate_via_host,
    make_generator,
    make_http_generator,
)


class _Handler(BaseHTTPRequestHandler):
    status = 200
    seen: ClassVar[dict[str, Any]] = {}

    def do_POST(self) -> None:
        length = int(self.headers["Content-Length"])
        _Handler.seen = json.loads(self.rfile.read(length))
        _Handler.seen["path"] = self.path
        body = json.dumps(
            {"text": "an answer", "model": "m", "load_secs": 0.0, "generate_secs": 1.0}
            if self.status == 200
            else {"detail": "no"}
        ).encode()
        self.send_response(self.status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    @override
    def log_message(self, format: str, *args: object) -> None:
        """Quiet: the test's output is the assertions, not an access log."""


@pytest.fixture
def host() -> Iterator[str]:
    server = HTTPServer(("127.0.0.1", 0), _Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_port}"
    finally:
        server.shutdown()
        thread.join(timeout=5)
        server.server_close()


def test_sends_the_whole_request_and_returns_the_text(host: str) -> None:
    _Handler.status = 200

    text = generate_via_host(
        "what happened?", base_url=host, system="be brief", model="m", max_tokens=99
    )

    assert text == "an answer"
    assert _Handler.seen == {
        "prompt": "what happened?",
        "system": "be brief",
        "model": "m",
        "max_tokens": 99,
        "path": "/generate",
    }


def test_generator_shape_hides_the_transport(host: str) -> None:
    _Handler.status = 200

    assert make_http_generator(host)("hello") == "an answer"
    assert _Handler.seen["model"] == DEFAULT_LLM


def test_a_refusing_holder_is_an_error_not_a_fallback(host: str) -> None:
    _Handler.status = 500

    with pytest.raises(LlmHostUnavailable, match="HTTP 500"):
        generate_via_host("hi", base_url=host)


def test_an_absent_holder_says_which_agent_is_missing() -> None:
    # Port 1 is reserved and never listening: a connection refused, not a hang.
    with pytest.raises(LlmHostUnavailable, match="recall-llm-host"):
        generate_via_host("hi", base_url="http://127.0.0.1:1")


def test_make_generator_prefers_the_holder(
    host: str, monkeypatch: pytest.MonkeyPatch
) -> None:
    _Handler.status = 200
    monkeypatch.setenv("RECALL_LLM_HOST", host)

    assert make_generator()("hello") == "an answer"


def test_empty_host_is_the_deliberate_in_process_escape_hatch(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    loaded: list[str] = []

    def fake_load(model: str) -> ChatModel:
        loaded.append(model)

        def chat(*, system: str | None, prompt: str, max_tokens: int) -> str:
            return "local"

        return chat

    monkeypatch.setenv("RECALL_LLM_HOST", "")
    monkeypatch.setattr("recall.llm.load_mlx_chat", fake_load)

    assert make_generator("m")("hello") == "local"
    assert loaded == ["m"]
