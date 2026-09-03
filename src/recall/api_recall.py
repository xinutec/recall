"""The recall-layer HTTP surface: vocabulary, household context, day
summaries (including today's stale-while-revalidate refresh), and Ask.

Slice 10, the last of api.py's decomposition (#1342). Module-level handlers
(the ask/summary tests patch _llm and the refresh indirection directly),
registrar-set state. `_start_today_refresh`'s ad-hoc thread is the one
background worker in the app that does not ride the queue rails — deliberate:
today's summary is stale-while-revalidate for a page the user is looking at
RIGHT NOW, and a queue hop through the refine daemon would trade seconds of
staleness for minutes.
"""

from __future__ import annotations

import logging
import threading
from collections.abc import Callable
from datetime import UTC, datetime

from fastapi import FastAPI, HTTPException

from recall import capture_control
from recall.api_models import AskIn, ContextIn, VocabularyIn
from recall.api_reads import transcript_out
from recall.ask import build_ask_prompt, retrieve
from recall.context import CONTEXT_KEY, household_context_block
from recall.llm import DEFAULT_LLM, Generator, make_generator
from recall.schemas import (
    AskOut,
    ContextOut,
    DaySummariesOut,
    NewIdOut,
    OkOut,
    TodaySummaryOut,
    TranscriptOut,
    VocabularyOut,
)
from recall.store import Store
from recall.summarize import refresh_live_summary

_log = logging.getLogger("recall.api")

# How long a queued ask may stay pending before the poll reports a timeout instead
# of spinning forever. Generous: the Mac path is a couple of 60s job cycles plus a
# possible cold model load, so only a genuine stall (Mac offline/wedged) should
# ever hit this.
_ASK_TIMEOUT_SECONDS = 600

_store_factory: Callable[[], Store] | None = None
_require_time_fn: Callable[[str | None], datetime] | None = None


def _store() -> Store:
    assert _store_factory is not None, "register_recall_routes was never called"
    return _store_factory()


def _require_time(value: str | None) -> datetime:
    assert _require_time_fn is not None
    return _require_time_fn(value)


def register_recall_routes(
    app: FastAPI,
    *,
    store_factory: Callable[[], Store],
    require_time: Callable[[str | None], datetime],
) -> None:
    """Mount the recall-layer surface."""
    global _store_factory, _require_time_fn  # noqa: PLW0603 - the registrar's one job
    _store_factory = store_factory
    _require_time_fn = require_time
    app.get("/api/vocabulary")(vocabulary)
    app.post("/api/vocabulary")(vocabulary_add)
    app.delete("/api/vocabulary/{term_id}")(vocabulary_delete)
    app.get("/api/context")(context_get)
    app.put("/api/context")(context_put)
    app.get("/api/summaries")(summaries)
    app.get("/api/summaries/today")(summary_today)
    app.post("/api/ask")(ask)
    app.get("/api/ask/{request_id}")(ask_status)


# The ask/summary generator. The weights are NOT in this process: they live in
# the llm-host (recall.llmhost), which holds one copy for everything on the Mac
# that wants it and releases it when idle. Indirected through _generator() so
# tests inject a stub without touching the host.
_llm: Generator | None = None


def _generator() -> Generator:
    global _llm  # noqa: PLW0603 - deliberate process-wide client cache
    if _llm is None:
        _llm = make_generator(DEFAULT_LLM)
    return _llm


def vocabulary() -> VocabularyOut:
    """The household vocabulary — terms the ASR is biased toward (recall.vocabulary).
    Applied on the next transcription after a change; no restart involved."""
    store = _store()
    try:
        return {
            "items": [{"id": t.id, "term": t.term} for t in store.vocabulary_terms()]
        }
    finally:
        store.close()


def vocabulary_add(body: VocabularyIn) -> NewIdOut:
    store = _store()
    try:
        return {"newId": store.add_vocabulary_term(body.term)}
    except ValueError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc
    finally:
        store.close()


def vocabulary_delete(term_id: int) -> OkOut:
    store = _store()
    try:
        store.delete_vocabulary_term(term_id)
        return {"ok": True}
    finally:
        store.close()


def context_get() -> ContextOut:
    """The household context — background facts prepended to the LLM prompts
    (summaries + ask). Stored in the DB, edited on the Labels page; the repo
    itself stays PII-free."""
    store = _store()
    try:
        return {"text": store.get_setting(CONTEXT_KEY) or ""}
    finally:
        store.close()


def context_put(body: ContextIn) -> OkOut:
    """Replace the household context. Applies from the next generation — and
    invalidates the cached today-summary so it regenerates with the new facts."""
    store = _store()
    try:
        store.set_setting(CONTEXT_KEY, body.text)
        # The cached today-summary was generated with the OLD context; mark it
        # stale (an impossible watermark) so the next look regenerates it.
        day = datetime.now(UTC).strftime("%Y-%m-%d")
        cached = store.get_live_summary(day)
        if cached is not None:
            store.set_live_summary(
                day, cached.text, model=cached.model, watermark="context-changed"
            )
    finally:
        store.close()
    return {"ok": True}


def summaries(limit: int = 7) -> DaySummariesOut:
    """Recent per-day summaries, newest first (generated by the refine daemon)."""
    store = _store()
    try:
        return {
            "items": [
                {"day": day, "text": text, "model": model}
                for day, text, model in store.recent_day_summaries(limit=limit)
            ]
        }
    finally:
        store.close()


# The "today so far" refresh: at most one background generation at a time. The
# summary is regenerated only when new turns landed since the last one (the
# watermark check below) and only when someone actually looks — event-driven,
# never on a timer.
_today_refresh_lock = threading.Lock()
_today_refreshing = False


def _refresh_today_worker(day: str) -> None:
    global _today_refreshing  # noqa: PLW0603 - paired with _start_today_refresh
    try:
        store = _store()
        try:
            refresh_live_summary(store, _generator(), day, model_name=DEFAULT_LLM)
        finally:
            store.close()
    except Exception:
        _log.exception("today-summary refresh failed")
    finally:
        with _today_refresh_lock:
            _today_refreshing = False


def _start_today_refresh(day: str) -> None:
    """Kick off one background regeneration; a no-op if one is already running.
    Indirected so tests can run it synchronously."""
    global _today_refreshing  # noqa: PLW0603 - single-flight guard
    with _today_refresh_lock:
        if _today_refreshing:
            return
        _today_refreshing = True
    threading.Thread(
        target=_refresh_today_worker, args=(day,), name="today-summary", daemon=True
    ).start()


def summary_today() -> TodaySummaryOut:
    """The running day's so-far summary, stale-while-revalidate: serve whatever is
    cached immediately; if new turns landed since it was generated, kick off one
    background regeneration and say so (upToDate/pending). The next request after
    it lands gets the fresh text. Quiet page loads cost nothing — the cache is
    keyed on the day's newest turn id, not a timer."""
    day = datetime.now(UTC).strftime("%Y-%m-%d")
    store = _store()
    try:
        watermark = store.day_watermark(day)
        cached = store.get_live_summary(day)
    finally:
        store.close()
    if watermark is None:  # nothing recorded yet today
        return {
            "day": day,
            "text": None,
            "generatedAt": None,
            "upToDate": True,
            "pending": False,
        }
    fresh = cached is not None and cached.watermark == watermark
    if not fresh:
        _start_today_refresh(day)
    return {
        "day": day,
        "text": cached.text if cached else None,
        "generatedAt": cached.generated_utc if cached else None,
        "upToDate": fresh,
        "pending": not fresh,
    }


def _ask_done(answer: str | None, sources: list[TranscriptOut]) -> AskOut:
    return {
        "status": "done",
        "id": None,
        "answer": answer,
        "sources": sources,
        "error": None,
    }


def ask(body: AskIn) -> AskOut:
    """Answer a question from the archive, grounded in retrieved turns.

    Retrieval + prompt-building happen here (the store + FTS live here). Generation is
    the only MLX-bound step: on the Mac the model is local, so it runs inline; on the
    fleet (Isis has no MLX) the built prompt is queued for the Mac and this returns a
    poll id — GET /api/ask/{id} resolves once the Mac lands the answer. Either way the
    cited turns come back immediately so the UI can show its sources while it waits."""
    question = body.question.strip()
    if not question:
        raise HTTPException(status_code=400, detail="question must not be blank")
    store = _store()
    try:
        turns = retrieve(store, question)
        if not turns:
            # No evidence: answer honestly now (no generation) — same on both hosts.
            return _ask_done(None, [])
        prompt = build_ask_prompt(
            question, turns, context=household_context_block(store)
        )
        sources = [transcript_out(t) for t in turns]
        if capture_control.is_fleet():
            rid = store.add_ask_request(question, prompt, [t.id for t in turns])
            return {
                "status": "pending",
                "id": rid,
                "answer": None,
                "sources": sources,
                "error": None,
            }
        return _ask_done(_generator()(prompt).strip(), sources)
    finally:
        store.close()


def ask_status(request_id: int) -> AskOut:
    """Poll a queued ask job (the fleet path): pending until the Mac's LLM lands the
    answer or an error. The cited sources are returned throughout."""
    store = _store()
    try:
        state = store.get_ask_request(request_id)
        if state is None:
            raise HTTPException(status_code=404, detail="unknown ask request")
        sources = [transcript_out(t) for t in store.turns_by_id(list(state.sources))]
        if not state.done:
            # Non-silent: a job the Mac never answers (Mac offline, stuck, wedged) would
            # otherwise spin "Thinking…" forever. After a generous backstop, surface a
            # timeout the UI can show. The row is left intact — a late answer still
            # lands for a fresh ask; this only stops one poll hanging indefinitely.
            age = (datetime.now(UTC) - state.created).total_seconds()
            if age > _ASK_TIMEOUT_SECONDS:
                return {
                    "status": "error",
                    "id": None,
                    "answer": None,
                    "sources": sources,
                    "error": "Timed out — the archive is busy or the Mac is "
                    "unreachable. Please try again.",
                }
            return {
                "status": "pending",
                "id": request_id,
                "answer": None,
                "sources": sources,
                "error": None,
            }
        if state.error is not None:
            return {
                "status": "error",
                "id": None,
                "answer": None,
                "sources": sources,
                "error": state.error,
            }
        return _ask_done(state.answer, sources)
    finally:
        store.close()
