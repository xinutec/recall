"""Local text generation for the recall layer (summaries, ask-the-archive).

Same shape as the ASR side: a `Generator` Protocol the pure logic is written
against, and one heavy factory that lazily imports mlx-lm and loads a quantised
instruct model on Metal — 100% local, matching the design's no-cloud rule. Tests
inject plain functions; only the factory touches ML.

**Where the weights actually live.** A 4-bit 7B is ~4.3 GB resident, and more
than one process wants it: this project's summaries and Ask, and life's emotion
suggestions. Loaded in-process by each, the Mac holds two copies — and the refine
daemon's copy is never released, so the second one is permanent. So the weights
live in ONE holder process (`recall llm-host`, recall.llmhost) and everything
else asks it over localhost. `make_generator` is the single place that choice is
made; it points at the holder unless `RECALL_LLM_HOST` is set to the empty
string, which means "load it in this process".
"""

from __future__ import annotations

import json
import logging
import os
import urllib.error
import urllib.request
from typing import Any, Protocol

_log = logging.getLogger("recall.llm")

# Qwen2.5 7B (4-bit): strong EN+NL instruction following, ~4.5 GB resident — fits
# alongside Whisper on the M4/32GB. Overridable per call site (--llm flags).
DEFAULT_LLM = "mlx-community/Qwen2.5-7B-Instruct-4bit"

# Summaries/answers are short; a bound keeps a runaway generation from pinning
# the GPU for minutes on a bad prompt.
MAX_TOKENS = 600

# Where the holder listens. Both ends of the contract live here (rather than in
# recall.llmhost) because this module is import-light: the capture agents run a
# python with no fastapi, and they import the CLI.
#
# Loopback only, and unauthenticated on purpose: all it can do is generate text
# from a prompt the caller already holds, and the interface IS the boundary.
LLM_HOST_BIND = "127.0.0.1"
LLM_HOST_PORT = 8092
DEFAULT_LLM_HOST = f"http://{LLM_HOST_BIND}:{LLM_HOST_PORT}"

# Five minutes of quiet and the weights go back: long enough to keep a run of
# requests warm, short enough that an unused evening costs nothing.
DEFAULT_IDLE_UNLOAD = 300.0

# A cold load is ~60s, a long answer tens of seconds more, and the holder
# serialises callers so a request can queue behind one. Generous enough that a
# legitimately slow answer is never cut off, finite so a wedge is still noticed.
HOST_TIMEOUT_SECONDS = 600.0


class Generator(Protocol):
    """Anything that turns a prompt into generated text."""

    def __call__(self, prompt: str, /) -> str: ...


class ChatModel(Protocol):
    """A loaded model, addressed the way instruct models want to be: a system
    message, a user message, and a token bound chosen by the call site."""

    def __call__(self, *, system: str | None, prompt: str, max_tokens: int) -> str: ...


class LlmHostUnavailable(RuntimeError):
    """The holder could not be reached, or refused the request.

    Fatal by design rather than falling back to an in-process load: the fallback
    would silently re-create the second copy of the weights this arrangement
    exists to prevent.
    """


def load_mlx_chat(model: str = DEFAULT_LLM) -> ChatModel:
    """Load `model` (lazy heavy import) and return a chat-templated generator."""
    from mlx_lm import generate, load  # noqa: PLC0415 - heavy ML import stays lazy

    # load()'s return type is a union (a 3-tuple only when return_config=True);
    # narrow the 2-tuple explicitly for mypy.
    loaded = load(model)
    llm, tokenizer = loaded[0], loaded[1]

    def run(*, system: str | None, prompt: str, max_tokens: int) -> str:
        messages = [{"role": "user", "content": prompt}]
        if system:
            messages.insert(0, {"role": "system", "content": system})
        # TokenizerWrapper delegates to the underlying HF tokenizer, which is
        # untyped — narrow the one boundary value instead of waiving the module.
        templated: str = tokenizer.apply_chat_template(  # type: ignore[no-untyped-call]
            messages, add_generation_prompt=True
        )
        result: str = generate(llm, tokenizer, prompt=templated, max_tokens=max_tokens)
        # Prompt length is the number that explains a slow answer: a long
        # few-shot prefix is read on every call, and reading it dominates
        # generating the reply. It is also what sizes a future prefix KV cache
        # (~56 KB/token for this model: 28 layers, 4 GQA KV heads, 128 dim).
        _log.info(
            "prompt %d tokens -> %d generated",
            len(tokenizer.encode(templated)),
            len(tokenizer.encode(result)),
        )
        return result

    return run


def make_mlx_generator(model: str = DEFAULT_LLM) -> Generator:
    """A prompt-only Generator backed by a model loaded into THIS process.

    Only the holder should normally call this (see the module docstring); other
    call sites go through `make_generator`.
    """
    chat = load_mlx_chat(model)

    def run(prompt: str, /) -> str:
        return chat(system=None, prompt=prompt, max_tokens=MAX_TOKENS)

    return run


def generate_via_host(
    prompt: str,
    *,
    base_url: str = DEFAULT_LLM_HOST,
    system: str | None = None,
    model: str = DEFAULT_LLM,
    max_tokens: int = MAX_TOKENS,
) -> str:
    """Ask the holder to generate. Raises `LlmHostUnavailable` if it cannot."""
    body: dict[str, Any] = {
        "prompt": prompt,
        "system": system,
        "model": model,
        "max_tokens": max_tokens,
    }
    request = urllib.request.Request(
        f"{base_url.rstrip('/')}/generate",
        data=json.dumps(body).encode(),
        method="POST",
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(request, timeout=HOST_TIMEOUT_SECONDS) as response:
            payload = json.loads(response.read())
    except urllib.error.HTTPError as exc:
        detail = exc.read().decode(errors="replace")[:500]
        raise LlmHostUnavailable(
            f"llm-host returned HTTP {exc.code}: {detail}"
        ) from exc
    except (urllib.error.URLError, TimeoutError, OSError) as exc:
        raise LlmHostUnavailable(
            f"no llm-host at {base_url} ({exc}) - "
            "is org.xinutec.recall-llm-host running?"
        ) from exc
    text: str = payload["text"]
    return text


def make_http_generator(
    base_url: str = DEFAULT_LLM_HOST, model: str = DEFAULT_LLM
) -> Generator:
    """A Generator that generates in the holder process instead of this one."""

    def run(prompt: str, /) -> str:
        return generate_via_host(prompt, base_url=base_url, model=model)

    return run


def make_generator(model: str = DEFAULT_LLM) -> Generator:
    """The Generator every caller outside the holder should use.

    Points at the holder by default. `RECALL_LLM_HOST=""` means "load it here" —
    the escape hatch for a machine with no holder agent. Any other value is a
    base URL.
    """
    host = os.environ.get("RECALL_LLM_HOST", DEFAULT_LLM_HOST)
    if not host:
        return make_mlx_generator(model)
    return make_http_generator(host, model)
