"""The model-shim protocol: one JSON request per line on stdin, one response
per line on stdout.

Python is kept only where a model is called (docs/architecture.md). A shim
holds the weights and touches nothing but its stdio and the audio path it is
given; the Rust runner owns the queue, fetching and retries. Lines, so a shim
can be driven by hand from a terminal.

stdout is the protocol. mlx-whisper and its dependencies print progress, so
`serve` points `sys.stdout` at stderr and writes responses to a private
handle.
"""

from __future__ import annotations

import json
import math
import os
import sys
import traceback
from collections.abc import Callable, Iterator
from typing import TextIO

#: What crosses the wire. Not `Any`, so an unserialisable result is a type
#: error, not a crash mid-stream.
type JsonValue = (
    str | int | float | bool | list[JsonValue] | dict[str, JsonValue] | None
)
type JsonDict = dict[str, JsonValue]

Handler = Callable[[str, "JsonDict"], "JsonValue"]

HELLO = "hello"


def parse_request(line: str) -> tuple[str | None, str | None, JsonDict]:
    """`(id, op, args)` for one request line; `op` is None when unusable.
    Never raises: dying on one bad line would throw away the loaded weights.
    """
    try:
        message = json.loads(line)
    except (ValueError, TypeError):
        return None, None, {}
    if not isinstance(message, dict):
        return None, None, {}
    ident = message.get("id")
    op = message.get("op")
    args = {k: v for k, v in message.items() if k not in ("id", "op")}
    return (
        str(ident) if ident is not None else None,
        str(op) if isinstance(op, str) else None,
        args,
    )


def finite(value: JsonValue) -> JsonValue:
    """Replace every non-finite float with `None`, recursively.

    `json.dumps` writes bare `NaN` and `Infinity`, which Python reads back but
    `serde_json` refuses, losing the whole reply. Whisper produces them on
    quiet or clipped audio (`avg_logprob`, word probabilities).
    `allow_nan=False` would instead fail the job over one score.
    """
    if isinstance(value, float):
        return None if math.isnan(value) or math.isinf(value) else value
    if isinstance(value, dict):
        return {k: finite(v) for k, v in value.items()}
    if isinstance(value, list):
        return [finite(v) for v in value]
    return value


def ok(ident: str | None, result: JsonValue) -> str:
    return json.dumps(
        {"id": ident, "ok": True, "result": finite(result)}, ensure_ascii=False
    )


def fail(ident: str | None, error: str) -> str:
    """An error response, so the runner can record the job as failed and move
    on."""
    return json.dumps({"id": ident, "ok": False, "error": error}, ensure_ascii=False)


def _protocol_stream() -> TextIO:
    """A private handle on the real stdout, taken before anything can print."""
    duplicated = os.dup(sys.stdout.fileno())
    sys.stdout.flush()
    return os.fdopen(duplicated, "w", buffering=1, encoding="utf-8")


def responses(lines: Iterator[str], handler: Handler, *, name: str) -> Iterator[str]:
    """Requests to responses, without I/O. `hello` is answered here, not by
    the handler, so it works even when the model failed to load.
    """
    for line in lines:
        if not line.strip():
            continue
        ident, op, args = parse_request(line)
        if op is None:
            yield fail(ident, "unparsable request")
            continue
        if op == HELLO:
            yield ok(ident, {"shim": name})
            continue
        try:
            yield ok(ident, handler(op, args))
        except Exception as err:  # answered, not raised
            yield fail(ident, f"{type(err).__name__}: {err}")
            print(traceback.format_exc(), file=sys.stderr)


def serve(handler: Handler, *, name: str) -> None:
    """Run the loop on stdio until stdin closes."""
    protocol = _protocol_stream()
    sys.stdout = sys.stderr
    for response in responses(iter(sys.stdin), handler, name=name):
        protocol.write(response + "\n")
        protocol.flush()
