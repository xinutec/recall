"""Two routes that outlived the features they were filed under: the household
vocabulary, and the on-demand refine.

Both were collateral of the 2026-09-06 scope cut (architecture.md, "Scope of the
rebuilt product"). Vocabulary lived in the recall-layer module because Ask and the
summaries read it into their prompts; refine lived in the experiments module
because A/B comparisons queue work the same way. Neither is an LLM feature and
neither is an experiment — they were rehomed rather than deleted, and the tests
that caught the mistake are why.

Both are core:

* **Vocabulary** is the household's proper nouns, applied as Whisper's
  `initial_prompt` on every pass (`recall.vocabulary`). It is the cheap
  accuracy lever for requirement #2, it is edited on the Labels page, and the
  Rust runner refuses to transcribe without it (#1463).
* **Refine** queues a diarized re-derivation of one stretch — the timeline's
  "Refine this section". It runs no ML inline; the idle-gated daemon executes
  it, so the heavy pass stays off live capture.
"""

from __future__ import annotations

from collections.abc import Callable
from datetime import datetime

from fastapi import FastAPI, HTTPException

from recall.api_models import RefineRequestIn, VocabularyIn
from recall.schemas import NewIdOut, OkOut, VocabularyOut
from recall.store import Store


def register_work_routes(
    app: FastAPI,
    *,
    store_factory: Callable[[], Store],
    require_time: Callable[[str | None], datetime],
) -> None:
    """Mount /api/vocabulary* and /api/refine."""

    @app.get("/api/vocabulary")
    def vocabulary() -> VocabularyOut:
        """The terms the ASR is biased toward. Applied on the next transcription
        after a change; no restart involved."""
        store = store_factory()
        try:
            return {
                "items": [
                    {"id": t.id, "term": t.term} for t in store.vocabulary_terms()
                ]
            }
        finally:
            store.close()

    @app.post("/api/vocabulary")
    def vocabulary_add(body: VocabularyIn) -> NewIdOut:
        store = store_factory()
        try:
            return {"newId": store.add_vocabulary_term(body.term)}
        except ValueError as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc
        finally:
            store.close()

    @app.delete("/api/vocabulary/{term_id}")
    def vocabulary_delete(term_id: int) -> OkOut:
        store = store_factory()
        try:
            store.delete_vocabulary_term(term_id)
            return {"ok": True}
        finally:
            store.close()

    @app.post("/api/refine")
    def refine_request(body: RefineRequestIn) -> OkOut:
        """Queue an on-demand diarize-refine of [start, end) of a recording."""
        try:
            start = require_time(body.start)
            end = require_time(body.end)
        except ValueError as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc
        store = store_factory()
        try:
            store.add_refine_request(body.source, start, end)
        finally:
            store.close()
        return {"ok": True}
