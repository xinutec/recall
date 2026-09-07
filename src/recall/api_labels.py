"""The labelling HTTP surface: the train queue, corrections, per-turn edits,
span assignment, voices, split, and the "sounds like" hint.

Slice 8 of api.py's decomposition (#1342). Module-level handlers (train and
suggest are called directly by tests) with registrar-set module state; the
clip-window helper is injected because it still lives in api.py with the
audio-serving family.
"""

from __future__ import annotations

from collections.abc import Callable
from datetime import UTC, datetime

from fastapi import FastAPI

from recall.api_models import (
    AssignSpanIn,
)
from recall.conversation import assign_span
from recall.schemas import (
    AssignResultOut,
)
from recall.store import Store

# Train pre-fills "sounds like X" only when the leading candidate's likelihood
# (softmax over the enrolled people) clears this — a confirmable hint, not a coin
# flip. The timeline still shows every guess with its %.
_store_factory: Callable[[], Store] | None = None
_parse_iso_fn: Callable[[str | None], datetime | None] | None = None
_require_time_fn: Callable[[str | None], datetime] | None = None


def _store() -> Store:
    assert _store_factory is not None, "register_label_routes was never called"
    return _store_factory()


def _parse_iso(value: str | None) -> datetime | None:
    assert _parse_iso_fn is not None
    return _parse_iso_fn(value)


def _require_time(value: str | None) -> datetime:
    assert _require_time_fn is not None
    return _require_time_fn(value)


def register_label_routes(
    app: FastAPI,
    *,
    store_factory: Callable[[], Store],
    parse_iso: Callable[[str | None], datetime | None],
    require_time: Callable[[str | None], datetime],
) -> None:
    """Mount the labelling surface. Dependencies land in module state."""
    global _store_factory, _parse_iso_fn, _require_time_fn  # noqa: PLW0603
    _store_factory = store_factory
    _parse_iso_fn = parse_iso
    _require_time_fn = require_time
    app.post("/api/sessions/{source}/assign")(assign)


CANT_MAKE_OUT_REASON = "can't make out (human)"
# Confidence band only *gathers* candidates (below the near-certain ceiling, above
# a floor that drops obvious junk). The queue is then ranked by *measured audio
# loudness*, because confidence is a poor proxy for "can a human label this" —
# the real signal is SNR: loud/close speech is labelable, quiet far-field isn't.
_TRAIN_MIN_CONFIDENCE = 0.30
_TRAIN_MAX_CONFIDENCE = 0.95
_TRAIN_CANDIDATES = 80
# A run of back-to-back turns this long is treated as TV/film (deprioritised) —
# the family's own speech is burstier and shorter than a movie's solid dialogue.
_MEDIA_MAX_GAP_S = 20.0
_MEDIA_MIN_DURATION_S = 480.0


def assign(source: str, body: AssignSpanIn) -> AssignResultOut:
    """Assign a text span (across turns, with partial edges) to a speaker — the one
    gesture behind reassign / split / merge. Returns the number of turns touched."""
    store = _store()
    try:
        touched = assign_span(
            store,
            source,
            body.startTurn,
            body.startChar,
            body.endTurn,
            body.endChar,
            body.name.strip(),
            now=datetime.now(UTC),
        )
    finally:
        store.close()
    return {"touched": touched}
