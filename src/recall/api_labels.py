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

from fastapi import FastAPI, HTTPException

from recall.api_models import (
    AssignSpanIn,
    CorrectIn,
    ReassignIn,
    TurnSpeakerIn,
)
from recall.conversation import assign_span
from recall.review import apply_correction
from recall.schemas import (
    AssignResultOut,
    NewIdOut,
    OkOut,
    SuggestOut,
    VoiceSuggestionsOut,
)
from recall.store import Store

# Train pre-fills "sounds like X" only when the leading candidate's likelihood
# (softmax over the enrolled people) clears this — a confirmable hint, not a coin
# flip. The timeline still shows every guess with its %.
_SUGGEST_MIN_PROB = 0.4
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
    app.post("/api/correct")(correct)
    app.post("/api/turn/{segment_id}/speaker")(turn_speaker)
    app.post("/api/sessions/{source}/assign")(assign)
    app.get("/api/sessions/{source}/voices")(voice_suggestions)
    app.post("/api/correction/{correction_id}/speaker")(correction_reassign)
    app.post("/api/correction/{correction_id}/hide")(correction_hide)
    app.get("/api/suggest/{segment_id}")(suggest)


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


def correct(body: CorrectIn) -> NewIdOut:
    store = _store()
    try:
        new_id = apply_correction(
            store,
            body.id,
            body.text,
            now=datetime.now(UTC),
            speaker=body.speaker,
            start=_parse_iso(body.start),
            end=_parse_iso(body.end),
            language=body.language,
        )
        return {"newId": new_id}
    except ValueError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc
    finally:
        store.close()


def turn_speaker(segment_id: int, body: TurnSpeakerIn) -> OkOut:
    """Reassign a single turn to a voice (or clear it) — for the spots diarization
    split onto the wrong speaker. Display label only."""
    name = (body.name or "").strip() or None
    store = _store()
    try:
        store.set_turn_speaker(segment_id, name)
    finally:
        store.close()
    return {"ok": True}


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


def voice_suggestions(source: str) -> VoiceSuggestionsOut:
    """Auto-suggested name per diarization voice in a session, from cached voiceprint
    guesses — so an enrolled household member is identified for you (the clinician you
    name by hand). `{cluster: name}`, only the confident, unambiguous ones."""
    store = _store()
    try:
        return {"suggestions": store.session_voice_suggestions(source)}
    finally:
        store.close()


def correction_reassign(correction_id: int, body: ReassignIn) -> OkOut:
    """Fix a mis-tagged label's voice (and its voiceprint + timeline segment)."""
    store = _store()
    try:
        store.set_correction_speaker(correction_id, body.speaker)
        return {"ok": True}
    finally:
        store.close()


def correction_hide(correction_id: int) -> OkOut:
    """Soft-remove a bad label from the corpus, counts, and its voiceprint."""
    store = _store()
    try:
        store.hide_correction(correction_id, "review")
        return {"ok": True}
    finally:
        store.close()


def suggest(segment_id: int) -> SuggestOut:
    """Best-matching enrolled name for a turn (or null) — powers the labelling
    "sounds like X" hint. Reads the cached guess (kept fresh by the worker's
    re-match against current voiceprints), so it agrees with the timeline and
    needs no live embedding. Returns the name only when the match is confident
    enough to pre-fill (a confirmable hint), else null.
    """
    store = _store()
    try:
        segment = store.get_transcript(segment_id)
        if segment is None:
            raise HTTPException(status_code=404, detail="unknown segment")
        # speaker_score is now a softmax likelihood across the enrolled people; only
        # pre-fill when the leading candidate is clearly ahead (a confirmable hint).
        confident = (segment.speaker_score or 0.0) >= _SUGGEST_MIN_PROB
        return {"speaker": segment.speaker_guess if confident else None}
    finally:
        store.close()
