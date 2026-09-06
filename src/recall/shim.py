"""The model-shim protocol (stage E2): JSON over stdio, one job at a time.

Python survives in this architecture only where a model is called
(docs/architecture.md, principle 6). A shim is that survival: a long-lived
process holding weights, reading one request per line and writing one response
per line, doing no I/O beyond its stdio and the audio path it is handed. The
Rust runner owns the queue, the fetching and the retries; the shim owns nothing.

Line-delimited JSON rather than a framed protocol: a shim's traffic is a handful
of messages per minute, and being able to drive one by hand from a terminal is
worth more here than bytes on the wire.

⚠ **stdout is the protocol, so nothing else may touch it.** mlx-whisper and its
dependencies print progress and warnings, and a single stray line would desync
the stream — silently, because JSON parsing would then fail on data that looks
almost right. `serve` redirects `sys.stdout` to stderr for the whole run and
keeps a private handle for responses, so a library print becomes a log line
instead of a corruption.
"""

from __future__ import annotations

import json
import os
import sys
import traceback
from collections.abc import Callable, Iterator
from typing import TextIO

#: What crosses the wire. Spelled out rather than `Any`, so a handler returning
#: something unserialisable is a type error here and not a crash mid-stream.
type JsonValue = (
    str | int | float | bool | None | list[JsonValue] | dict[str, JsonValue]
)
type JsonDict = dict[str, JsonValue]

#: What a handler is: an op name and its arguments, returning a JSON-able result.
Handler = Callable[[str, "JsonDict"], "JsonValue"]

HELLO = "hello"


def parse_request(line: str) -> tuple[str | None, str | None, JsonDict]:
    """`(id, op, args)` for one request line; `op` is None when unusable.

    Never raises: a malformed line is a message the caller answers with an
    error, not a reason to take the process down. A shim that dies on one bad
    request loses the weights it spent seconds loading.
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


def ok(ident: str | None, result: JsonValue) -> str:
    return json.dumps({"id": ident, "ok": True, "result": result}, ensure_ascii=False)


def fail(ident: str | None, error: str) -> str:
    """An error is a RESPONSE, not an exception. The runner must be able to ack
    a job that cannot be done and move on; a shim that crashes instead turns one
    bad clip into a stalled queue."""
    return json.dumps({"id": ident, "ok": False, "error": error}, ensure_ascii=False)


def _protocol_stream() -> TextIO:
    """A private handle on the real stdout, taken before anything can print."""
    duplicated = os.dup(sys.stdout.fileno())
    sys.stdout.flush()
    return os.fdopen(duplicated, "w", buffering=1, encoding="utf-8")


def responses(lines: Iterator[str], handler: Handler, *, name: str) -> Iterator[str]:
    """Pure request/response mapping — no I/O, so the protocol is unit-tested.

    `hello` is answered here rather than by the handler: the runner uses it to
    learn a shim is alive and what it is, and that must work even for a shim
    whose model failed to load.
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
        except Exception as err:  # any model failure is a RESPONSE, not a crash
            yield fail(ident, f"{type(err).__name__}: {err}")
            print(traceback.format_exc(), file=sys.stderr)


def serve(handler: Handler, *, name: str) -> None:
    """Run the loop on stdio until stdin closes."""
    protocol = _protocol_stream()
    # Everything else in the process now logs; only `protocol` is the wire.
    sys.stdout = sys.stderr
    for response in responses(iter(sys.stdin), handler, name=name):
        protocol.write(response + "\n")
        protocol.flush()
