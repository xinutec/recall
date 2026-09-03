"""The uploaded-sessions HTTP surface: list, upload, rename, delete,
re-diarize, transcript export, and per-session voice naming.

Slice 5 of api.py's decomposition (#1342), closure style like slices 1-3;
the tests that used to call two of these handlers directly now drive the
transport instead.
"""

from __future__ import annotations

import shutil
import subprocess
from collections.abc import Callable
from datetime import UTC, datetime
from pathlib import Path
from zoneinfo import ZoneInfo

from fastapi import FastAPI, File, Form, HTTPException, UploadFile

from recall.api_models import SessionRenameIn, VoiceNameIn
from recall.probe import probe_media
from recall.schemas import OkOut, SessionOut, SessionsOut, TranscriptExportOut
from recall.sources import AudioSource, SourceKind
from recall.store import Store
from recall.timeline import Segment
from recall.transcript_view import clean_transcript

_MEETING_ZONE = ZoneInfo("Europe/London")
# Containers a conversation recording might arrive in (phone voice memos are m4a;
# most recorders export mp3). ffprobe still validates the actual content.
_UPLOAD_AUDIO_SUFFIXES = frozenset(
    {".mp3", ".m4a", ".mp4", ".wav", ".flac", ".aac", ".ogg", ".opus", ".webm"}
)


def register_session_routes(
    app: FastAPI,
    *,
    store_factory: Callable[[], Store],
    data_root: Callable[[], Path],
    require_time: Callable[[str | None], datetime],
) -> None:
    """Mount /api/sessions*. Dependencies injected as in every slice."""
    _register_list_and_upload(app, store_factory, data_root, require_time)
    _register_session_management(app, store_factory, data_root)
    _register_voice_route(app, store_factory)


def _register_list_and_upload(
    app: FastAPI,
    store_factory: Callable[[], Store],
    data_root: Callable[[], Path],
    require_time: Callable[[str | None], datetime],
) -> None:
    @app.get("/api/sessions")
    def sessions() -> SessionsOut:
        """Discrete uploaded recordings (e.g. doctor meetings) as a dated list to
        browse;
        each one opens in the timeline filtered to that session."""
        store = store_factory()
        try:
            rows = store.session_summaries()
        finally:
            store.close()
        return {
            "items": [
                {
                    "id": sid,
                    "title": name,
                    "start": start,
                    "end": end,
                    "turnCount": turns,
                    "speakers": sorted(speakers.split(",")) if speakers else [],
                }
                for sid, name, start, end, turns, speakers in rows
            ]
        }

    @app.post("/api/sessions")
    def create_session(
        audio: UploadFile = File(...),
        title: str = Form(""),
        start: str = Form(""),
    ) -> SessionOut:
        """Upload a discrete conversation recording (e.g. a hospital appointment) as a
        new
        session. Streams the file to its own dir under DATA_ROOT (container kept as-is,
        not
        forced to WAV), registers it as an UPLOAD source, and returns the session —
        which
        appears in the list at once (0 turns) while the worker transcribes it and the
        idle
        refine daemon diarizes it. `start` is the recording's local start (ISO); else
        now.
        """
        try:
            parsed = require_time(start) if start else datetime.now(UTC)
        except ValueError as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc
        started = parsed.astimezone(UTC)
        suffix = Path(audio.filename or "").suffix.lower()
        if suffix not in _UPLOAD_AUDIO_SUFFIXES:
            raise HTTPException(
                status_code=400, detail=f"unsupported audio type {suffix!r}"
            )
        local = started.astimezone(_MEETING_ZONE)
        source_id = f"meeting-{local:%Y%m%d-%H%M}"
        name = title.strip() or f"Meeting {local:%Y-%m-%d %H:%M}"
        out_dir = data_root() / source_id
        out_dir.mkdir(parents=True, exist_ok=True)
        path = out_dir / f"{source_id}-{started:%Y%m%dT%H%M%S}{suffix}"
        with path.open("wb") as fh:
            shutil.copyfileobj(audio.file, fh)
        try:
            duration, sample_rate, channels = probe_media(path)
        except (subprocess.CalledProcessError, KeyError, IndexError, ValueError) as exc:
            path.unlink(missing_ok=True)
            raise HTTPException(status_code=400, detail="could not read audio") from exc
        store = store_factory()
        try:
            # register, not add: an upload KNOWS what it is, and the worker may already
            # have discovered this directory (it scans the data root continuously). An
            # INSERT OR IGNORE here would leave the guess standing and the session would
            # never appear in the list.
            store.register_source(
                AudioSource(id=source_id, name=name, kind=SourceKind.UPLOAD, spec="")
            )
            store.add_audio_segment(
                Segment(
                    source_id=source_id,
                    sequence=0,
                    start=started,
                    end=started + duration,
                    path=str(path),
                    sample_rate=sample_rate,
                    channels=channels,
                )
            )
        finally:
            store.close()
        return {
            "id": source_id,
            "title": name,
            "start": started.isoformat(),
            "end": (started + duration).isoformat(),
            "turnCount": 0,
            "speakers": [],
        }


def _register_session_management(
    app: FastAPI,
    store_factory: Callable[[], Store],
    data_root: Callable[[], Path],
) -> None:
    def _require_upload(store: Store, source: str) -> None:
        """Guard: rename/delete/re-diarize act on uploaded meetings only — never on the
        continuous household capture (which is append-only and never deleted)."""
        kind = store.source_kind(source)
        if kind is None:
            raise HTTPException(status_code=404, detail="no such session")
        if kind != SourceKind.UPLOAD:
            raise HTTPException(status_code=400, detail="not an uploaded session")

    @app.patch("/api/sessions/{source}")
    def rename_session(source: str, body: SessionRenameIn) -> OkOut:
        """Rename an uploaded session (its list title)."""
        title = body.title.strip()
        if not title:
            raise HTTPException(status_code=400, detail="title required")
        store = store_factory()
        try:
            _require_upload(store, source)
            store.rename_source(source, title)
        finally:
            store.close()
        return {"ok": True}

    @app.delete("/api/sessions/{source}")
    def delete_session(source: str) -> OkOut:
        """Delete an uploaded session — its turns, audio segments, queued work, and
        files.
        Guarded to UPLOAD sources so household capture can't be erased through this
        path."""
        store = store_factory()
        try:
            _require_upload(store, source)
            paths = store.delete_source(source)
        finally:
            store.close()
        for p in paths:
            Path(p).unlink(missing_ok=True)
        parent = data_root() / source
        if parent.is_dir():
            shutil.rmtree(parent, ignore_errors=True)
        return {"ok": True}

    @app.post("/api/sessions/{source}/rediarize")
    def rediarize_session(source: str) -> OkOut:
        """Re-derive who-said-what for a whole session. Queues an idle-gated refine
        (never
        runs pyannote inline — that would starve live capture), spanning the full
        recording.
        """
        store = store_factory()
        try:
            _require_upload(store, source)
            span = store.source_span(source)
            if span is None:
                raise HTTPException(status_code=400, detail="session has no audio")
            store.add_refine_request(source, span[0], span[1])
        finally:
            store.close()
        return {"ok": True}


def _register_voice_route(app: FastAPI, store_factory: Callable[[], Store]) -> None:
    @app.post("/api/sessions/{source}/voice")
    def name_voice(source: str, body: VoiceNameIn) -> OkOut:
        """Human-name a diarization voice across a session — labels every turn of that
        voice at once. Writes no correction, but the label feeds the voiceprint backfill
        (`speaker_label` is its work-list), so the named voice becomes matchable — a
        meeting's clinician is enrolled like any household voice."""
        name = (body.name or "").strip() or None
        store = store_factory()
        try:
            store.name_voice(source, body.cluster, name)
        finally:
            store.close()
        return {"ok": True}

    @app.get("/api/sessions/{source}/transcript")
    def session_transcript(source: str) -> TranscriptExportOut:
        """A session's clean, finalised transcript for export to a doc/website:
        consecutive
        same-speaker turns merged into one bubble, each with its local start time and
        the
        display speaker; current/corrected state only; deterministic. Identical
        to the
        CLI's
        `transcript --json`. Meant to render into a marker-delimited section of
        a markdown
        page, re-run on demand without touching the manually-maintained parts."""
        store = store_factory()
        try:
            return clean_transcript(source, store.session_turns(source))
        finally:
            store.close()
