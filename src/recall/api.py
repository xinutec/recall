"""FastAPI JSON API over the recall core (consumed by the Angular front-end).

A thin transport layer: every endpoint calls the typed core (store / review /
speakerid) and returns JSON. FastAPI/pydantic are waived in mypy (present only in
the venv), so keep logic in the core — not here. When a built Angular app exists
at frontend/dist/, it's served as static files so everything is one origin.
"""

from __future__ import annotations

import logging
import threading
from datetime import UTC, datetime
from pathlib import Path

from fastapi import FastAPI, HTTPException
from fastapi.responses import FileResponse

from recall import capture_control
from recall.api_audio import clip_window, register_audio_routes
from recall.api_capture import fleet_capture_state, register_capture_routes
from recall.api_client_reports import register_client_report_routes
from recall.api_devices import register_device_routes
from recall.api_experiments import register_experiment_routes
from recall.api_labels import register_label_routes
from recall.api_models import (
    AskIn,
    ContextIn,
    VocabularyIn,
)
from recall.api_quiet import register_quiet_routes
from recall.api_reads import register_read_routes
from recall.api_reads import transcript_out as _transcript
from recall.api_sessions import register_session_routes
from recall.ask import build_ask_prompt, retrieve
from recall.context import CONTEXT_KEY, household_context_block
from recall.llm import DEFAULT_LLM, Generator, make_generator
from recall.paths import default_data_root
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
from recall.store import (
    Store,
)
from recall.summarize import refresh_live_summary
from recall.sync import register_sync_routes
from recall.webauth import (
    register_web_auth,
)

DATA_ROOT = default_data_root()
_log = logging.getLogger("recall.api")
# Train pre-fills "sounds like X" only when the leading candidate's likelihood
# (softmax over the enrolled people) clears this — a confirmable hint, not a coin
# flip. The timeline still shows every guess with its %.
_SUGGEST_MIN_PROB = 0.4
# How long a queued ask may stay pending before the poll reports a timeout instead of
# spinning forever. Generous: the Mac path is a couple of 60s job cycles plus a possible
# cold model load, so only a genuine stall (Mac offline/wedged) should ever hit this.
_ASK_TIMEOUT_SECONDS = 600
_REPO = Path(__file__).resolve().parent.parent.parent
_FRONTEND = _REPO / "frontend" / "dist" / "recall-web" / "browser"


app = FastAPI(title="recall")


def _store() -> Store:
    return Store.open(DATA_ROOT / "recall.sqlite")


# Mac→fleet sync endpoints for the proposed Isis split (recall.sync). Inert unless
# RECALL_SYNC_TOKEN is set — register_sync_routes adds nothing and returns False — so a
# stock LAN-only deployment is unchanged.
register_sync_routes(app, _store, DATA_ROOT)

# Nextcloud SSO gate over the human-facing web UI (recall.webauth). Also inert unless
# configured (RECALL_SESSION_SECRET + NC_CLIENT_ID + NC_CLIENT_SECRET), so the Mac's
# LAN-only UI stays open; only the Isis fleet pod, where the secret lives, raises the
# wall. The recording plane (/sync/* and the iOS mic app's capture endpoints) is exempt.
register_web_auth(app)


register_read_routes(app, store_factory=_store, parse_iso=lambda v: _parse_iso(v))  # noqa: PLW0108 - forward ref
register_client_report_routes(app, client_log_path=_REPO / "logs" / "client.log")


register_capture_routes(app, store_factory=_store, data_root=lambda: DATA_ROOT)
register_device_routes(
    app,
    store_factory=_store,
    data_root=lambda: DATA_ROOT,
    fleet_capture_state=fleet_capture_state,
)
register_session_routes(
    app,
    store_factory=_store,
    data_root=lambda: DATA_ROOT,
    # a lambda, deliberately: _require_time is defined further down this module
    require_time=lambda value: _require_time(value),  # noqa: PLW0108 - forward ref
)


def _parse_iso(value: str | None) -> datetime | None:
    if value is None:
        return None
    parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    return parsed if parsed.tzinfo else parsed.replace(tzinfo=UTC)


def _require_time(value: str | None) -> datetime:
    parsed = _parse_iso(value)
    if parsed is None:
        msg = "a valid ISO 8601 time is required"
        raise ValueError(msg)
    return parsed


register_label_routes(
    app,
    store_factory=_store,
    parse_iso=_parse_iso,
    require_time=_require_time,
    clip_window_fn=clip_window,
)
register_audio_routes(app, store_factory=_store)
register_experiment_routes(
    app,
    store_factory=_store,
    require_time=_require_time,
    parse_iso=_parse_iso,
)
register_quiet_routes(app, store_factory=_store, require_time=_require_time)


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


@app.get("/api/vocabulary")
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


@app.post("/api/vocabulary")
def vocabulary_add(body: VocabularyIn) -> NewIdOut:
    store = _store()
    try:
        return {"newId": store.add_vocabulary_term(body.term)}
    except ValueError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc
    finally:
        store.close()


@app.delete("/api/vocabulary/{term_id}")
def vocabulary_delete(term_id: int) -> OkOut:
    store = _store()
    try:
        store.delete_vocabulary_term(term_id)
        return {"ok": True}
    finally:
        store.close()


@app.get("/api/context")
def context_get() -> ContextOut:
    """The household context — background facts prepended to the LLM prompts
    (summaries + ask). Stored in the DB, edited on the Labels page; the repo
    itself stays PII-free."""
    store = _store()
    try:
        return {"text": store.get_setting(CONTEXT_KEY) or ""}
    finally:
        store.close()


@app.put("/api/context")
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


@app.get("/api/summaries")
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


@app.get("/api/summaries/today")
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


@app.post("/api/ask")
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
        sources = [_transcript(t) for t in turns]
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


@app.get("/api/ask/{request_id}")
def ask_status(request_id: int) -> AskOut:
    """Poll a queued ask job (the fleet path): pending until the Mac's LLM lands the
    answer or an error. The cited sources are returned throughout."""
    store = _store()
    try:
        state = store.get_ask_request(request_id)
        if state is None:
            raise HTTPException(status_code=404, detail="unknown ask request")
        sources = [_transcript(t) for t in store.turns_by_id(list(state.sources))]
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


def _frontend_file(rel: str) -> Path | None:
    """Resolve `rel` to a real file inside the built frontend, or None.

    Guards against path traversal: the resolved path must stay under _FRONTEND.
    """
    if not _FRONTEND.is_dir():
        return None
    candidate = (_FRONTEND / rel).resolve()
    root = _FRONTEND.resolve()
    if candidate != root and root not in candidate.parents:
        return None
    return candidate if candidate.is_file() else None


@app.get("/{full_path:path}")
def spa(full_path: str) -> FileResponse:
    """Serve built assets; fall back to index.html so client-side routes work.

    Registered last, so the explicit /api/* routes above always win. API paths
    that fall through here are genuine misses and return 404 (not index.html).
    """
    if full_path.startswith("api/"):
        raise HTTPException(status_code=404, detail="not found")
    asset = _frontend_file(full_path)
    if asset is not None:
        # Built assets are content-hashed (main-<hash>.js), so they're immutable —
        # cache them hard.
        return FileResponse(
            asset,
            headers={"Cache-Control": "public, max-age=31536000, immutable"},
        )
    index = _frontend_file("index.html")
    if index is None:
        raise HTTPException(status_code=404, detail="frontend not built")
    # index.html names the current hashed bundles, so it must never be cached — else a
    # deploy isn't picked up until a hard refresh (the bug that served stale code).
    return FileResponse(index, headers={"Cache-Control": "no-cache"})
