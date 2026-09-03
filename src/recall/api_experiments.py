"""The experiment HTTP surface: on-demand refines and A/B model comparisons.

Slice 6 of api.py's decomposition (#1342). Both queue work the Mac's idle-gated
refine daemon executes; nothing here runs ML inline.
"""

from __future__ import annotations

import json
from collections.abc import Callable
from datetime import datetime
from typing import cast

from fastapi import FastAPI, HTTPException

from recall.api_models import AbCompareStartIn, RefineRequestIn
from recall.asr import DEFAULT_MODEL
from recall.finetune import DEFAULT_BASE_MODEL
from recall.schemas import (
    AbCompareRunOut,
    AbCompareRunsOut,
    AbCompareRunSummaryOut,
    AbCompareScoreOut,
    AbCompareSegmentDiffOut,
    AbCompareStatus,
    NewIdOut,
    OkOut,
)
from recall.store import AbCompareJob, Store


def register_experiment_routes(
    app: FastAPI,
    *,
    store_factory: Callable[[], Store],
    require_time: Callable[[str | None], datetime],
    parse_iso: Callable[[str | None], datetime | None],
) -> None:
    """Mount /api/refine + /api/ab-compare*."""

    @app.post("/api/refine")
    def refine_request(body: RefineRequestIn) -> OkOut:
        """Queue an on-demand diarize-refine of [start, end) of a recording. The idle-
        gated
        refine daemon runs it, so the heavy pass stays off live capture — the timeline's
        'Refine this' action."""
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

    @app.post("/api/ab-compare")
    def ab_compare_start(body: AbCompareStartIn) -> NewIdOut:
        """Queue a non-destructive A/B comparison of two ASR models over a recording.
        The
        refine daemon runs it; poll `GET /api/ab-compare/{id}` for the result."""
        try:
            frm = parse_iso(body.frm or None)
            to = parse_iso(body.to or None)
        except ValueError as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc
        store = store_factory()
        try:
            run_id = store.add_ab_compare_run(
                body.source,
                frm,
                to,
                # The default adapter is stored by its machine-independent NAME: the row
                # crosses the Isis split (queued here, executed on the Mac), so another
                # machine's absolute path would be meaningless. The Mac resolves it
                # against its own data root at run time (cli._resolve_model).
                model_a=body.modelA or DEFAULT_MODEL,
                model_b=body.modelB or "adapter-current",
                base_model=body.baseModel or DEFAULT_BASE_MODEL,
            )
        finally:
            store.close()
        return {"newId": run_id}

    @app.get("/api/ab-compare")
    def ab_compare_runs() -> AbCompareRunsOut:
        """All A/B comparison runs, newest first (summaries only)."""
        store = store_factory()
        try:
            return {"items": [_ab_run_summary(j) for j in store.list_ab_compare_runs()]}
        finally:
            store.close()

    @app.get("/api/ab-compare/{run_id}")
    def ab_compare_run(run_id: int) -> AbCompareRunOut:
        """One run in full: its summary plus the per-span WER evidence (each with the
        audio of that span) and the whole-segment text diffs. Lists are empty until
        done."""
        store = store_factory()
        try:
            job = store.get_ab_compare_run(run_id)
        finally:
            store.close()
        if job is None:
            raise HTTPException(status_code=404, detail="no such run")
        scores, diffs = _ab_run_detail(job)
        return {
            "summary": _ab_run_summary(job),
            "scores": scores,
            "segmentDiffs": diffs,
        }


def _ab_run_summary(job: AbCompareJob) -> AbCompareRunSummaryOut:
    return {
        "id": job.id,
        "source": job.source,
        "modelA": job.model_a,
        "modelB": job.model_b,
        "baseModel": job.base_model,
        "status": cast(AbCompareStatus, job.status),
        "created": job.created.isoformat(),
        "meanWerA": job.mean_wer_a,
        "meanWerB": job.mean_wer_b,
        "nCorrections": job.n_corrections,
        "nSegments": job.n_segments,
        "nChanged": job.n_changed,
        "error": job.error,
    }


def _ab_run_detail(
    job: AbCompareJob,
) -> tuple[list[AbCompareScoreOut], list[AbCompareSegmentDiffOut]]:
    """Parse a finished run's stored report into the per-span scores (each carrying the
    audio URL of its corrected span) and the whole-segment diffs. Empty if not done."""
    if not job.result_json:
        return [], []
    report = cast("dict[str, object]", json.loads(job.result_json))
    raw_scores = cast("list[dict[str, object]]", report.get("correction_scores", []))
    raw_diffs = cast("list[dict[str, object]]", report.get("segment_diffs", []))
    scores: list[AbCompareScoreOut] = [
        {
            "correctionId": cast(int, s["correction_id"]),
            "truth": cast(str, s["truth"]),
            "textA": cast(str, s["text_a"]),
            "textB": cast(str, s["text_b"]),
            "werA": cast(float, s["wer_a"]),
            "werB": cast(float, s["wer_b"]),
            "audioUrl": f"/api/correction/{cast(int, s['correction_id'])}/audio",
        }
        for s in raw_scores
    ]
    diffs: list[AbCompareSegmentDiffOut] = [
        {
            "audioId": cast(int, d["audio_id"]),
            "start": cast(str, d["start"]),
            "changed": cast(bool, d["changed"]),
            "textA": cast(str, d["text_a"]),
            "textB": cast(str, d["text_b"]),
        }
        for d in raw_diffs
    ]
    return scores, diffs
