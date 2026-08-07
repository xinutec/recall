"""The one process that holds the LLM weights, so no other process has to.

A 4-bit 7B is ~4.3 GB of unified memory, which on this Mac is memory Whisper and
the capture pipeline also want. Two consumers wanted it — recall's summaries and
Ask, and life's emotion suggestions — and each used to load its own copy, with
recall's never released once created. This daemon is the fix: it owns the
weights, serves generation over localhost, and lets go after a stretch of quiet.

Three properties it exists to guarantee:

- **One copy.** Everything else is an HTTP client (`recall.llm.make_generator`).
  Asking for a different model swaps the resident one rather than adding to it.
- **One at a time.** Generation is serialised by a lock: one GPU, and two
  concurrent generations would only make both slower while doubling the working
  set. Callers queue.
- **It lets go.** After `idle_unload` seconds without a request the weights are
  dropped and MLX's buffer cache handed back, so a feature used a few times a day
  doesn't hold gigabytes all day. The next request pays the reload (~60s cold),
  which nobody waits on: recall's summaries are a background drain and life's
  suggestions are cached and read later.

Localhost-only by default, and unauthenticated on purpose: the only thing it can
do is generate text from a prompt the caller already has, and binding to the
loopback interface is the boundary. Do NOT bind it to 0.0.0.0.
"""

from __future__ import annotations

import logging
import threading
import time
from collections.abc import Callable
from dataclasses import dataclass

from fastapi import FastAPI
from pydantic import BaseModel, Field

from recall.llm import (
    DEFAULT_IDLE_UNLOAD,
    DEFAULT_LLM,
    LLM_HOST_BIND,
    LLM_HOST_PORT,
    MAX_TOKENS,
    ChatModel,
    load_mlx_chat,
)

_log = logging.getLogger("recall.llmhost")

# How often the reaper looks. Fine-grained enough that "5 minutes" means it.
REAP_INTERVAL = 15.0


@dataclass(frozen=True)
class Generated:
    """What a generation cost, as well as what it produced — a caller that waited
    60s should be able to see that it paid for a load, not a slow model."""

    text: str
    model: str
    load_secs: float
    generate_secs: float


class ModelHolder:
    """Holds at most one loaded model, and releases it when it goes unused.

    `loader` and `clock` are injected so the whole thing is testable without ML:
    the only ML in this module is the default loader.
    """

    def __init__(
        self,
        *,
        default_model: str = DEFAULT_LLM,
        idle_unload: float = DEFAULT_IDLE_UNLOAD,
        loader: Callable[[str], ChatModel] = load_mlx_chat,
        clock: Callable[[], float] = time.monotonic,
    ) -> None:
        self._default_model = default_model
        self._idle_unload = idle_unload
        self._loader = loader
        self._clock = clock
        self._lock = threading.Lock()
        self._model: str | None = None
        self._chat: ChatModel | None = None
        # None, not 0.0: "never used" must not be representable as a number, or
        # `clock() - never` reads as a real duration and the sweeper drops the
        # weights (dev-lint python-zero-timestamp-sentinel).
        self._last_used: float | None = None
        self._generations = 0
        # What is being loaded right now, and since when. Written under the lock,
        # read without it — a caller queued behind a load must be able to learn
        # that is what it is waiting for, and taking the lock to ask would be the
        # very wait it is trying to describe.
        self._loading: str | None = None
        self._loading_since: float | None = None

    def generate(
        self,
        prompt: str,
        *,
        system: str | None = None,
        model: str | None = None,
        max_tokens: int = MAX_TOKENS,
    ) -> Generated:
        """Generate, loading or swapping the resident model if needed. Serialised."""
        wanted = model or self._default_model
        with self._lock:
            load_secs = 0.0
            if self._chat is None or self._model != wanted:
                if self._chat is not None:
                    _log.info("swapping %s -> %s", self._model, wanted)
                    self._drop()
                started = self._clock()
                _log.info("loading %s", wanted)
                self._loading = wanted
                self._loading_since = started
                try:
                    self._chat = self._loader(wanted)
                finally:
                    self._loading = None
                    self._loading_since = None
                self._model = wanted
                # Stamped on load, not only on a successful generation: a model
                # that has just been loaded is not idle, even if the request that
                # loaded it goes on to fail. Leaving it at 0 made the reaper read
                # the whole process uptime as idle time and drop the weights
                # instantly, so the next request paid a fresh cold load.
                self._last_used = self._clock()
                load_secs = self._clock() - started
                _log.info("loaded %s in %.1fs", wanted, load_secs)
            started = self._clock()
            try:
                text = self._chat(system=system, prompt=prompt, max_tokens=max_tokens)
            finally:
                # Same reason: time spent on a failed generation is still use.
                self._last_used = self._clock()
            generate_secs = self._clock() - started
            self._generations += 1
        return Generated(
            text=text, model=wanted, load_secs=load_secs, generate_secs=generate_secs
        )

    def release_if_idle(self) -> bool:
        """Drop the weights if nothing has used them for `idle_unload` seconds.

        Never blocks: a generation in flight holds the lock, and a model being
        used right now is by definition not idle.
        """
        if not self._lock.acquire(blocking=False):
            return False
        try:
            if self._chat is None or self._last_used is None:
                return False
            idle = self._clock() - self._last_used
            if idle < self._idle_unload:
                return False
            _log.info("releasing %s after %.0fs idle", self._model, idle)
            self._drop()
            return True
        finally:
            self._lock.release()

    def status(self) -> tuple[str | None, float | None, int]:
        """(resident model, seconds since last use, generations served).

        Read without the lock on purpose, so `/health` answers immediately while
        a long generation is in flight. Diagnostics, not a decision input.
        """
        if self._chat is None or self._last_used is None:
            return None, None, self._generations
        return self._model, self._clock() - self._last_used, self._generations

    def loading(self) -> tuple[str | None, float | None]:
        """(model being loaded, seconds it has been loading) — (None, None) when
        no load is in flight.

        Lock-free like `status`, and for a sharper reason: this is what tells a
        waiting caller that the silence is a load rather than a dead daemon.
        Measured loads run from 0.7s to 1533.6s on this machine, so "nothing is
        resident" and "your model is on its way" are very different answers and
        were previously indistinguishable from outside.
        """
        model, since = self._loading, self._loading_since
        if model is None or since is None:
            return None, None
        return model, self._clock() - since

    def _drop(self) -> None:
        """Caller holds the lock."""
        self._chat = None
        self._model = None
        _free_metal_cache()


def _free_metal_cache() -> None:
    """Hand MLX's cached GPU buffers back to the system.

    Dropping the last reference frees the arrays, but MLX keeps the underlying
    Metal allocations in its own cache for reuse — which is exactly what we are
    trying not to hold. Absent wherever mlx isn't installed (tests, the fleet),
    where there is no cache to free either.
    """
    try:
        import mlx.core as mx  # noqa: PLC0415 - lazy, and optional by design
    except ImportError:
        return
    mx.clear_cache()


class GenerateIn(BaseModel):
    prompt: str
    system: str | None = None
    model: str | None = None
    max_tokens: int = Field(default=MAX_TOKENS, gt=0, le=8192)


class GenerateOut(BaseModel):
    text: str
    model: str
    load_secs: float
    generate_secs: float


class HealthOut(BaseModel):
    model: str | None
    idle_secs: float | None
    generations: int
    # Set only while weights are being read. A caller whose request is queued
    # behind that can say so instead of reporting the host as unreachable.
    loading: str | None = None
    loading_secs: float | None = None


def build_app(holder: ModelHolder) -> FastAPI:
    app = FastAPI(title="recall llm-host")

    # Sync handlers: FastAPI runs them in a threadpool, so a generation holding
    # the holder's lock for a minute never blocks /health or a second request's
    # arrival — it just makes it wait its turn.
    @app.post("/generate")
    def generate(body: GenerateIn) -> GenerateOut:
        result = holder.generate(
            body.prompt,
            system=body.system,
            model=body.model,
            max_tokens=body.max_tokens,
        )
        return GenerateOut(
            text=result.text,
            model=result.model,
            load_secs=result.load_secs,
            generate_secs=result.generate_secs,
        )

    @app.get("/health")
    def health() -> HealthOut:
        model, idle_secs, generations = holder.status()
        loading, loading_secs = holder.loading()
        return HealthOut(
            model=model,
            idle_secs=idle_secs,
            generations=generations,
            loading=loading,
            loading_secs=loading_secs,
        )

    return app


def start_reaper(
    holder: ModelHolder, *, interval: float = REAP_INTERVAL
) -> threading.Event:
    """Release the model in the background once it goes idle.

    Returns the stop event. A daemon thread: an idle sweep is never a reason to
    keep the process alive.
    """
    stop = threading.Event()

    def loop() -> None:
        while not stop.wait(interval):
            holder.release_if_idle()

    threading.Thread(target=loop, name="llm-host-reaper", daemon=True).start()
    return stop


def serve(
    *,
    host: str = LLM_HOST_BIND,
    port: int = LLM_HOST_PORT,
    model: str = DEFAULT_LLM,
    idle_unload: float = DEFAULT_IDLE_UNLOAD,
) -> None:
    import uvicorn  # noqa: PLC0415 - keep the web stack out of other commands

    holder = ModelHolder(default_model=model, idle_unload=idle_unload)
    start_reaper(holder)
    _log.info(
        "llm-host on %s:%d — %s, released after %.0fs idle",
        host,
        port,
        model,
        idle_unload,
    )
    uvicorn.run(build_app(holder), host=host, port=port, log_level="info")
