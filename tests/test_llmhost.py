"""The model holder: one copy, one at a time, and it lets go."""

from __future__ import annotations

import threading

import pytest
from fastapi.testclient import TestClient

from recall.llm import ChatModel
from recall.llmhost import ModelHolder, build_app


class FakeClock:
    """A monotonic clock the test moves by hand."""

    def __init__(self) -> None:
        self.now = 1000.0

    def __call__(self) -> float:
        return self.now


class Loads:
    """A loader that records what it was asked for and how often."""

    def __init__(self, clock: FakeClock | None = None, load_secs: float = 0.0) -> None:
        self.names: list[str] = []
        self._clock = clock
        self._load_secs = load_secs

    def __call__(self, name: str) -> ChatModel:
        self.names.append(name)
        if self._clock is not None:
            self._clock.now += self._load_secs

        def chat(*, system: str | None, prompt: str, max_tokens: int) -> str:
            return f"{name}|{system}|{prompt}|{max_tokens}"

        return chat


def test_generate_loads_once_and_reports_what_it_cost() -> None:
    clock = FakeClock()
    loader = Loads(clock, load_secs=60.0)
    holder = ModelHolder(default_model="m", loader=loader, clock=clock)

    first = holder.generate("hello", system="be brief", max_tokens=42)

    assert first.text == "m|be brief|hello|42"
    assert first.model == "m"
    assert first.load_secs == 60.0

    second = holder.generate("again")

    assert second.load_secs == 0.0, "a warm model must not be reloaded"
    assert loader.names == ["m"]


def test_asking_for_another_model_swaps_rather_than_adds() -> None:
    loader = Loads()
    holder = ModelHolder(default_model="m", loader=loader, clock=FakeClock())

    holder.generate("a")
    holder.generate("b", model="other")

    assert loader.names == ["m", "other"]
    resident, _, generations = holder.status()
    assert resident == "other", "only the newest model may be resident"
    assert generations == 2


def test_released_only_once_genuinely_idle() -> None:
    clock = FakeClock()
    loader = Loads()
    holder = ModelHolder(
        default_model="m", idle_unload=300.0, loader=loader, clock=clock
    )
    holder.generate("a")

    clock.now += 299.0
    assert holder.release_if_idle() is False
    assert holder.status()[0] == "m"

    clock.now += 2.0
    assert holder.release_if_idle() is True
    assert holder.status() == (None, None, 1)

    holder.generate("b")
    assert loader.names == ["m", "m"], "the next request reloads it"


def test_a_failed_generation_does_not_make_the_model_look_idle_forever() -> None:
    """A model that has only ever failed is still freshly loaded.

    `_last_used` used to be stamped after a successful call only, so a failure on
    a just-loaded model left it at 0 — an "idle time" of the whole process
    uptime, and the reaper dropped ~4.3 GB of weights immediately. The next
    request then paid a full cold load. Seen for real: "releasing … after 934391s
    idle", seconds after loading.
    """
    clock = FakeClock()

    def loader(name: str) -> ChatModel:
        def chat(*, system: str | None, prompt: str, max_tokens: int) -> str:
            raise ValueError("bad prompt")

        return chat

    holder = ModelHolder(
        default_model="m", idle_unload=300.0, loader=loader, clock=clock
    )

    with pytest.raises(ValueError, match="bad prompt"):
        holder.generate("a")

    assert holder.release_if_idle() is False, "a fresh model is not idle"
    assert holder.status()[0] == "m"


def test_release_never_waits_on_a_generation_in_flight() -> None:
    """The reaper runs on a timer; a model being generated with is not idle."""
    started = threading.Event()
    finish = threading.Event()

    def loader(name: str) -> ChatModel:
        def chat(*, system: str | None, prompt: str, max_tokens: int) -> str:
            started.set()
            assert finish.wait(timeout=5), "generation never released"
            return "done"

        return chat

    holder = ModelHolder(default_model="m", idle_unload=0.0, loader=loader)
    worker = threading.Thread(target=lambda: holder.generate("a"))
    worker.start()
    try:
        assert started.wait(timeout=5)
        assert holder.release_if_idle() is False
    finally:
        finish.set()
        worker.join(timeout=5)


def test_generations_are_serialised() -> None:
    """One GPU: two concurrent generations would only make both slower."""
    overlapping = 0
    peak = 0
    guard = threading.Lock()

    def loader(name: str) -> ChatModel:
        def chat(*, system: str | None, prompt: str, max_tokens: int) -> str:
            nonlocal overlapping, peak
            with guard:
                overlapping += 1
                peak = max(peak, overlapping)
            try:
                return prompt
            finally:
                with guard:
                    overlapping -= 1

        return chat

    holder = ModelHolder(default_model="m", loader=loader)

    def ask(index: int) -> None:
        holder.generate(str(index))

    threads = [threading.Thread(target=ask, args=(i,)) for i in range(8)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join(timeout=5)

    assert peak == 1


def test_http_surface() -> None:
    holder = ModelHolder(default_model="m", loader=Loads(), clock=FakeClock())
    client = TestClient(build_app(holder))

    assert client.get("/health").json() == {
        "model": None,
        "idle_secs": None,
        "generations": 0,
    }

    body = client.post(
        "/generate", json={"prompt": "hi", "system": "sys", "max_tokens": 7}
    ).json()

    assert body["text"] == "m|sys|hi|7"
    assert body["model"] == "m"
    assert client.get("/health").json()["model"] == "m"


def test_http_rejects_an_unusable_token_bound() -> None:
    holder = ModelHolder(default_model="m", loader=Loads())
    client = TestClient(build_app(holder))

    response = client.post("/generate", json={"prompt": "hi", "max_tokens": 0})

    assert response.status_code == 422
