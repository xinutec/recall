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

import hashlib
import json
import logging
import os
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any, Protocol

_log = logging.getLogger("recall.llm")

# Qwen2.5 7B (4-bit): strong EN+NL instruction following, ~4.5 GB resident — fits
# alongside Whisper on the M4/32GB. Overridable per call site (--llm flags).
DEFAULT_LLM = "mlx-community/Qwen2.5-7B-Instruct-4bit"

# Summaries/answers are short; a bound keeps a runaway generation from pinning
# the GPU for minutes on a bad prompt.
MAX_TOKENS = 600

# Disk prefix-KV cache. A long, STABLE system prompt (emotion suggestions send
# ~5-6k tokens of vocabulary + a day-stable few-shot) is otherwise re-prefilled on
# every call, and that prefill is most of a warm request's time. When the same
# system recurs, its KV is computed once and saved to disk; the next call loads it
# and prefills only the changing user turn. The weights still unload on idle — the
# point is that the expensive PREFILL survives on disk across those reloads, with
# no resident RAM cost. Keyed by the system's content hash, so a new day's few-shot
# is a natural miss-and-rebuild. Skipped below the threshold: the disk round-trip
# only pays off past a few hundred tokens of prefix, and it never fires for the
# system-less Generator path (summaries/Ask), only system+user chat calls.
PREFIX_CACHE_MIN_TOKENS = 512
PREFIX_CACHE_TTL_SECS = 2 * 24 * 3600  # prune a cache untouched for two days

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
    import mlx.core as mx  # noqa: PLC0415 - heavy ML import stays lazy
    from mlx_lm import generate, load  # noqa: PLC0415
    from mlx_lm.models.cache import (  # noqa: PLC0415
        load_prompt_cache,
        make_prompt_cache,
        save_prompt_cache,
    )

    # load()'s return type is a union (a 3-tuple only when return_config=True);
    # narrow the 2-tuple explicitly for mypy.
    loaded = load(model)
    llm, tokenizer = loaded[0], loaded[1]

    def templated(messages: list[dict[str, str]]) -> list[int] | str:
        # TokenizerWrapper delegates to the untyped HF tokenizer. It returns TOKEN
        # IDS, not text (transformers only returns a string with tokenize=False) —
        # verify that shape at the boundary rather than trusting an annotation,
        # which read `str` for months without anyone noticing because mlx-lm's
        # generate accepts either.
        ids = tokenizer.apply_chat_template(  # type: ignore[no-untyped-call]
            messages, add_generation_prompt=True
        )
        if isinstance(ids, (list, str)):
            return ids
        raise TypeError(
            f"apply_chat_template returned {type(ids).__name__}, not tokens or text"
        )

    def prefix_cache(system: str, full: list[int]) -> tuple[list[int], object | None]:
        """Return (tokens_to_prefill, prompt_cache). On a hit the tokens are just
        the changing suffix and the cache holds the system's KV; on a miss the KV
        is built and saved and the suffix returned; on anything unexpected it falls
        back to the full prompt with no cache — the cache is speed, never an answer.
        """
        prefix_msg = [{"role": "system", "content": system}]
        prefix = tokenizer.apply_chat_template(prefix_msg)  # type: ignore[no-untyped-call]
        # The system turn is a literal token-prefix of the full prompt (verified,
        # not assumed — a template that injected a default or reordered would break
        # the split silently). Below the threshold the disk round-trip is a loss.
        if (
            not isinstance(prefix, list)
            or len(prefix) < PREFIX_CACHE_MIN_TOKENS
            or full[: len(prefix)] != prefix
        ):
            return full, None
        suffix = full[len(prefix) :]
        key = hashlib.sha256(system.encode("utf-8")).hexdigest()[:32]
        path = _prefix_cache_dir() / f"{key}.safetensors"
        try:
            if path.exists():
                cache = load_prompt_cache(str(path))  # type: ignore[no-untyped-call]
                path.touch()  # mark used, for TTL pruning
                _log.info(
                    "prefix cache HIT %s: prefill %d tok (was %d)",
                    key[:8],
                    len(suffix),
                    len(full),
                )
                return suffix, cache
            cache = make_prompt_cache(llm)
            llm(mx.array(prefix)[None], cache=cache)
            mx.eval([c.state for c in cache])
            save_prompt_cache(str(path), cache)
            _prune_prefix_caches()
            _log.info(
                "prefix cache BUILT %s: %d tok prefix saved", key[:8], len(prefix)
            )
            return suffix, cache
        except Exception as e:
            _log.warning("prefix cache unusable (%s); full prefill instead", e)
            return full, None

    def run(*, system: str | None, prompt: str, max_tokens: int) -> str:
        messages = [{"role": "user", "content": prompt}]
        if system:
            messages.insert(0, {"role": "system", "content": system})
        full = templated(messages)

        cache: object | None = None
        to_prefill: list[int] | str = full
        if system and isinstance(full, list):
            to_prefill, cache = prefix_cache(system, full)

        extra: dict[str, Any] = {"prompt_cache": cache} if cache is not None else {}
        result: str = generate(
            llm, tokenizer, prompt=to_prefill, max_tokens=max_tokens, **extra
        )
        # Prompt length is the number that explains a slow answer: the few-shot
        # prefix dominates, which is exactly what the disk cache above skips
        # re-reading on a hit.
        prompt_tokens = (
            len(full) if isinstance(full, list) else len(tokenizer.encode(full))
        )
        _log.info(
            "prompt %d tokens -> %d generated%s",
            prompt_tokens,
            len(tokenizer.encode(result)),
            " (cached prefix)" if cache is not None else "",
        )
        return result

    return run


def _prefix_cache_dir() -> Path:
    """Where disk prefix caches live. Overridable so a test or a second holder
    doesn't collide with the live one's directory."""
    d = Path(
        os.environ.get("RECALL_LLM_CACHE_DIR")
        or Path.home() / "Library/Caches/recall/llm-prefix"
    )
    d.mkdir(parents=True, exist_ok=True)
    return d


def _prune_prefix_caches() -> None:
    """Drop cache files untouched past the TTL — the key rotates daily (a new
    few-shot), so without this the directory would grow one ~300 MB file a day."""
    cutoff = time.time() - PREFIX_CACHE_TTL_SECS
    for f in _prefix_cache_dir().glob("*.safetensors"):
        try:
            if f.stat().st_mtime < cutoff:
                f.unlink(missing_ok=True)
        except OSError:  # a file vanishing under us is fine — it is gone
            pass


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
