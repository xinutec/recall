"""FastAPI JSON API over the recall core (consumed by the Angular front-end).

A thin transport layer: every endpoint calls the typed core (store / review /
speakerid) and returns JSON. FastAPI/pydantic are waived in mypy (present only in
the venv), so keep logic in the core — not here. When a built Angular app exists
at frontend/dist/, it's served as static files so everything is one origin.
"""

from __future__ import annotations

import hashlib
import json
import logging
import shutil
import subprocess
import tempfile
import threading
import time
import unicodedata
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import cast
from zoneinfo import ZoneInfo

from fastapi import FastAPI, File, Form, HTTPException, UploadFile
from fastapi.responses import FileResponse, Response

from recall import capture_control
from recall.api_models import (
    AbCompareStartIn,
    AskIn,
    AssignSpanIn,
    ClientLog,
    ContextIn,
    CorrectIn,
    HeartbeatIn,
    NudgeIn,
    OutboxIn,
    QuietDeleteIn,
    ReassignIn,
    RefineRequestIn,
    SessionRenameIn,
    SplitIn,
    TelemetryEvent,
    TurnSpeakerIn,
    UnhideIn,
    UnintelligibleIn,
    VocabularyIn,
    VoiceNameIn,
)
from recall.ask import build_ask_prompt, retrieve
from recall.asr import DEFAULT_MODEL, slice_clip
from recall.capture import alive_mtime
from recall.context import CONTEXT_KEY, household_context_block
from recall.conversation import assign_span
from recall.conversations import (
    DEFAULT_GAP_SECONDS,
    Conversation,
    segment_conversations,
)
from recall.finetune import DEFAULT_BASE_MODEL
from recall.ids import AudioSegmentId
from recall.liveness import source_statuses
from recall.llm import DEFAULT_LLM, Generator, make_generator
from recall.loudness import normalize_loudness
from recall.mic_alive import Beat, read_beats, record_beat
from recall.moments import Moment, best_colocated_guess, cluster_moments
from recall.outbox import OutboxReport, read_reports, record_report
from recall.paths import default_data_root
from recall.probe import probe_media
from recall.ranking import (
    diversity_factor,
    foreign_script_ratio,
    normalize_text,
    training_value,
)
from recall.review import (
    SpeakerFragment,
    apply_correction,
    review_queue,
    split_correction,
)
from recall.scan_job import ScanJob
from recall.schemas import (
    AbCompareRunOut,
    AbCompareRunsOut,
    AbCompareRunSummaryOut,
    AbCompareScoreOut,
    AbCompareSegmentDiffOut,
    AbCompareStatus,
    AroundOut,
    AskOut,
    AssignResultOut,
    CaptureOut,
    ContextOut,
    ConversationOut,
    ConversationsOut,
    CorrectionsOut,
    DaySummariesOut,
    EnvelopeOut,
    HeartbeatOut,
    HeartbeatsOut,
    ItemsOut,
    LabelOut,
    MomentOut,
    NewIdOut,
    NewIdsOut,
    OkOut,
    OutboxesOut,
    PageOut,
    QuietDeletedOut,
    QuietScanOut,
    QuietSpansOut,
    SessionOut,
    SessionsOut,
    SourcesOut,
    SpeakerNamesOut,
    SuggestOut,
    Tier,
    TodaySummaryOut,
    TrainOut,
    TranscriptExportOut,
    TranscriptOut,
    VocabularyOut,
    VoiceSuggestionsOut,
)
from recall.sources import DEVICE_KINDS, AudioSource, SourceKind, SourceRow
from recall.store import (
    DIARIZED_MARKER,
    HUMAN_MODEL,
    LIVE_MODEL,
    AbCompareJob,
    LabelledFragment,
    Store,
    TranscriptSegment,
)
from recall.summarize import refresh_live_summary
from recall.sync import register_sync_routes
from recall.timeline import Segment
from recall.transcript_view import clean_transcript
from recall.webauth import register_web_auth

DATA_ROOT = default_data_root()
_log = logging.getLogger("recall.api")
# The one background archive-measuring scan (see recall.scan_job), created on first use.
_SCAN_JOB: ScanJob | None = None
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
# Client-side (phone browser) errors are logged here so they're visible
# server-side — the phone has no console you can read.
_CLIENT_LOG = _REPO / "logs" / "client.log"
# Playback context around a transcript turn. Whisper splits a recording into
# short phrase-level turns; slicing exactly to one phrase yields a 1-2s clip with
# no context, which is jarring and useless for recall. Give each clip real
# lead-in/-out, and a minimum length so even one-word turns are listenable.
_AUDIO_PAD_S = 1.5
_AUDIO_MIN_S = 5.0
# A diarized turn is a precise per-speaker cutout, so it's played tight — just the
# turn plus a small safety pad so onsets/offsets aren't clipped (the diarization
# boundary is approximate) — instead of pulling in the neighbouring speaker.
_AUDIO_TIGHT_PAD_S = 0.2


def clip_window(
    phrase_start: float, phrase_end: float, *, pad: float, minimum: float
) -> tuple[float, float]:
    """Widen a [start, end] phrase span (seconds within its audio file) for playback.

    Adds `pad` on each side, then expands symmetrically to at least `minimum`
    seconds. Start is clamped at 0; the end may run past the file (ffmpeg stops
    at EOF).
    """
    start = phrase_start - pad
    end = phrase_end + pad
    if end - start < minimum:
        mid = (phrase_start + phrase_end) / 2.0
        start = mid - minimum / 2.0
        end = mid + minimum / 2.0
    return max(0.0, start), end


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


def _tier(segment: TranscriptSegment) -> Tier:
    """Which analysis tier produced this turn — surfaced as a UI badge so it's
    visible how much processing a turn has had: instant, basic, diarized, or human."""
    if segment.asr_model == HUMAN_MODEL:
        return "corrected"
    if segment.asr_model == LIVE_MODEL:
        return "live"
    if (segment.provenance or "").startswith(DIARIZED_MARKER):
        return "diarized"
    return "transcribed"


def _precise(segment: TranscriptSegment) -> bool:
    """A precise cutout — a diarized turn, or any turn carrying word timings (e.g. a
    span-assign split) — is played tight: just its span plus a tiny safety pad, not the
    wide context window a rough whole-phrase turn needs to be listenable."""
    return _tier(segment) == "diarized" or segment.word_timings is not None


def _transcript(
    segment: TranscriptSegment,
    *,
    guess: tuple[str | None, float | None] | None = None,
) -> TranscriptOut:
    # Speaker: a human label is authoritative (confirmed); otherwise show the
    # best auto guess with its match strength, so the UI can render "Alice 31%"
    # rather than hiding a weak-but-useful guess as "unknown". In a folded moment,
    # `guess` overrides with the strongest attribution among the co-located mics (the
    # same speech), so a spine chosen for transcription quality still shows the most
    # confident "who".
    confirmed = segment.speaker_label is not None
    if confirmed:
        speaker = segment.speaker_label
        speaker_confidence = None
    elif guess is not None:
        speaker, speaker_confidence = guess
    else:
        speaker, speaker_confidence = segment.speaker_guess, segment.speaker_score
    return {
        "id": segment.id,
        "start": segment.start.isoformat(),
        "end": segment.end.isoformat(),
        "text": segment.text,
        "language": segment.language,
        "speaker": speaker,
        "speakerConfirmed": confirmed,
        "speakerConfidence": speaker_confidence,
        "confidence": segment.asr_confidence,
        "loudness": segment.loudness,
        "model": segment.asr_model,
        "tier": _tier(segment),
        "hidden": segment.hidden_reason,
        "audioUrl": f"/api/audio/{segment.id}",
        "source": segment.source_id,
        "cluster": segment.speaker_cluster,
    }


@app.post("/api/log")
def client_log(body: ClientLog) -> OkOut:
    """Record a browser-side error/event to logs/client.log (phone has no console)."""
    stamp = datetime.now(UTC).isoformat(timespec="seconds")
    parts = [stamp, f"[{body.level}]", body.url or "-", body.message]
    if body.stack:
        parts.append(f"\n    {body.stack.splitlines()[0]}")
    _CLIENT_LOG.parent.mkdir(parents=True, exist_ok=True)
    with _CLIENT_LOG.open("a") as fh:
        fh.write(" ".join(parts) + "\n")
    return {"ok": True}


# A per-batch cap so a buggy client cannot turn one POST into a log flood, and a
# label cap so a pathological one cannot bloat a line. Counted in code points,
# not bytes, so a multi-byte glyph is never split.
_MAX_EVENTS = 100
_MAX_LABEL = 160


def _one_line(label: str, max_len: int) -> str:
    """Flatten a client-supplied label to a single harmless log field.

    The security boundary of the telemetry endpoint, not tidiness. A label is
    verbatim UI text written into a log line as ``label=…``, so a newline inside
    it forges *whole log lines* — including further ``client-event`` lines
    attributed to someone else. The log stops being evidence, which is the one
    thing it exists to be.

    ``str.split()`` with no argument splits on every Unicode whitespace,
    including the U+2028/U+2029 separators that are not control characters; the
    category pass ahead of it catches the format and control characters that are
    not whitespace at all.
    """
    unbroken = "".join(
        " " if unicodedata.category(c) in {"Cc", "Cf", "Zl", "Zp"} else c for c in label
    )
    return " ".join(unbroken.split())[:max_len]


@app.post("/api/telemetry")
def telemetry(events: list[TelemetryEvent]) -> OkOut:
    """Record what the person did, beside what the API was asked for.

    Distinct from ``/api/log`` above, which reports browser *errors* to a file.
    This is the activity trace: a tap that hits a cache, a control that was
    disabled, a screen that rendered wrong — none of it reaches the server
    otherwise, so "I pressed it and nothing happened" is undiagnosable.

    Goes to the application logger rather than ``logs/client.log`` deliberately,
    so it interleaves with the request log and a session reads as one timeline.
    There is no storage: these are logs, not data.

    Same ``client-event`` line shape as every other app in the fleet — the whole
    value is being able to grep one word anywhere and get the same fields.
    """
    for e in events[:_MAX_EVENTS]:
        label = _one_line(e.label or "", _MAX_LABEL)
        _log.info(
            "client-event kind=%s path=%s label=%s at=%s", e.kind, e.path, label, e.at
        )
    return {"ok": True}


@app.get("/api/sessions")
def sessions() -> SessionsOut:
    """Discrete uploaded recordings (e.g. doctor meetings) as a dated list to browse;
    each one opens in the timeline filtered to that session."""
    store = _store()
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


_MEETING_ZONE = ZoneInfo("Europe/London")
# Containers a conversation recording might arrive in (phone voice memos are m4a;
# most recorders export mp3). ffprobe still validates the actual content.
_UPLOAD_AUDIO_SUFFIXES = frozenset(
    {".mp3", ".m4a", ".mp4", ".wav", ".flac", ".aac", ".ogg", ".opus", ".webm"}
)


@app.post("/api/sessions")
def create_session(
    audio: UploadFile = File(...),
    title: str = Form(""),
    start: str = Form(""),
) -> SessionOut:
    """Upload a discrete conversation recording (e.g. a hospital appointment) as a new
    session. Streams the file to its own dir under DATA_ROOT (container kept as-is, not
    forced to WAV), registers it as an UPLOAD source, and returns the session — which
    appears in the list at once (0 turns) while the worker transcribes it and the idle
    refine daemon diarizes it. `start` is the recording's local start (ISO); else now.
    """
    try:
        parsed = _require_time(start) if start else datetime.now(UTC)
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
    out_dir = DATA_ROOT / source_id
    out_dir.mkdir(parents=True, exist_ok=True)
    path = out_dir / f"{source_id}-{started:%Y%m%dT%H%M%S}{suffix}"
    with path.open("wb") as fh:
        shutil.copyfileobj(audio.file, fh)
    try:
        duration, sample_rate, channels = probe_media(path)
    except (subprocess.CalledProcessError, KeyError, IndexError, ValueError) as exc:
        path.unlink(missing_ok=True)
        raise HTTPException(status_code=400, detail="could not read audio") from exc
    store = _store()
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
    store = _store()
    try:
        _require_upload(store, source)
        store.rename_source(source, title)
    finally:
        store.close()
    return {"ok": True}


@app.delete("/api/sessions/{source}")
def delete_session(source: str) -> OkOut:
    """Delete an uploaded session — its turns, audio segments, queued work, and files.
    Guarded to UPLOAD sources so household capture can't be erased through this path."""
    store = _store()
    try:
        _require_upload(store, source)
        paths = store.delete_source(source)
    finally:
        store.close()
    for p in paths:
        Path(p).unlink(missing_ok=True)
    parent = DATA_ROOT / source
    if parent.is_dir():
        shutil.rmtree(parent, ignore_errors=True)
    return {"ok": True}


@app.post("/api/sessions/{source}/rediarize")
def rediarize_session(source: str) -> OkOut:
    """Re-derive who-said-what for a whole session. Queues an idle-gated refine (never
    runs pyannote inline — that would starve live capture), spanning the full recording.
    """
    store = _store()
    try:
        _require_upload(store, source)
        span = store.source_span(source)
        if span is None:
            raise HTTPException(status_code=400, detail="session has no audio")
        store.add_refine_request(source, span[0], span[1])
    finally:
        store.close()
    return {"ok": True}


@app.get("/api/sessions/{source}/transcript")
def session_transcript(source: str) -> TranscriptExportOut:
    """A session's clean, finalised transcript for export to a doc/website: consecutive
    same-speaker turns merged into one bubble, each with its local start time and the
    display speaker; current/corrected state only; deterministic. Identical to the CLI's
    `transcript --json`. Meant to render into a marker-delimited section of a markdown
    page, re-run on demand without touching the manually-maintained parts."""
    store = _store()
    try:
        return clean_transcript(source, store.session_turns(source))
    finally:
        store.close()


def _local_last_active(
    rows: list[SourceRow], now: datetime
) -> dict[str, datetime | None]:
    """Liveness on the capturing host (the Mac), from each source's .alive marker —
    refreshed by the ingest pump while a phone streams real signal, and by the
    capture watchdog while the mic's closed segments decode to real audio
    (recall.capture.ALIVE_FILE) — so "active" means measured recording. The mic's
    marker is additionally gated on the pause state: its window is a leisurely
    ~75s (watchdog cadence), and a pause must read idle at once."""
    usb_recording = capture_control.capture_running() and not capture_control.is_paused(
        DATA_ROOT, now
    )
    last_active: dict[str, datetime | None] = {}
    for row in rows:
        marker = alive_mtime(DATA_ROOT / row.id)
        if row.kind is SourceKind.TCP_PCM:
            last_active[row.id] = marker
        else:
            last_active[row.id] = marker if usb_recording else None
    return last_active


def _fleet_last_active(
    store: Store, rows: list[SourceRow], now: datetime
) -> dict[str, datetime | None]:
    """Liveness on the fleet (Isis), which runs no capture or ingest and cannot see
    the Mac's markers. It comes entirely from the Mac's mirror report
    (recall.capture_mirror), which ships every source's .alive freshness — the same
    measured-recording signal the Mac serves locally, one report cadence older. The
    mic keeps its local pause gate. A quiet Mac (no fresh report) reads as no one
    live — correct: the fleet genuinely does not know, and fleetwatch covers a dead
    Mac separately."""
    usb_recording = _fleet_capture_state(store, now)["running"]
    reported = capture_control.reported_source_liveness(store, now) or {}
    last_active: dict[str, datetime | None] = {}
    for row in rows:
        if row.kind is SourceKind.TCP_PCM:
            last_active[row.id] = reported.get(row.id)
        else:
            last_active[row.id] = reported.get(row.id) if usb_recording else None
    return last_active


@app.get("/api/sources")
def sources() -> SourcesOut:
    """Per-recorder liveness for the fleet view: active iff the source's liveness
    marker — refreshed only on measured audio — is fresh, so a dot means recording,
    not connected. On the fleet (Isis), which runs no capture, the markers arrive via
    the Mac's ~5s mirror report; the windows widen to absorb the cadence
    (recall.liveness.active_window). Uploaded recordings (meetings) are sources but
    not live devices, so they're excluded — they live in the Sessions view."""
    now = datetime.now(UTC)
    on_fleet = capture_control.is_fleet()
    store = _store()
    try:
        rows = [r for r in store.source_rows() if r.kind in DEVICE_KINDS]
        last_active = (
            _fleet_last_active(store, rows, now)
            if on_fleet
            else _local_last_active(rows, now)
        )
    finally:
        store.close()
    statuses = source_statuses(rows, last_active, now, on_fleet=on_fleet)
    return {
        "items": [
            {
                "id": s.source_id,
                "name": s.name,
                "kind": s.kind.value,
                "active": s.active,
                "lastActive": s.last_active.isoformat() if s.last_active else None,
            }
            for s in statuses
        ]
    }


@app.post("/api/devices/outbox")
def report_outbox(body: OutboxIn) -> OkOut:
    """A phone says what it is still holding.

    The gap this closes: an approved recording the phone cannot deliver was state
    no fleet component could see. The meeting recorder 401ed from the day it was
    written, retried out to a +1h23m backoff, and said "N recordings waiting to
    upload" throughout — the same sentence it shows when you are simply not home
    yet (#77). The Mac's doctor posts to fleetwatch every five minutes and had no
    way to know.

    Best-effort status, never control: an unparseable time is dropped rather than
    refused, because a phone on an older build should cost its own line and
    nothing else.
    """
    now = datetime.now(UTC)
    store = _store()
    try:
        record_report(
            store,
            OutboxReport(
                device=body.device,
                queued=max(0, body.queued),
                oldest_queued_at=_iso_or_none(body.oldestQueuedAt),
                failing=max(0, body.failing),
                reason=body.reason,
                at=now,
            ),
        )
    finally:
        store.close()
    return {"ok": True}


@app.post("/api/devices/heartbeat")
def record_heartbeat(body: HeartbeatIn) -> OkOut:
    """A mic app says it is still running (#837).

    The gap this closes: recall could not tell a dead recorder from a quiet room.
    The liveness marker is refreshed only by audio above the silence floor — right
    for "is it recording", useless for "is it alive" — and while capture is paused
    the ingest listener is closed entirely, so nothing streams and nothing is known.
    Capture was paused for the four days before this was written.

    ⚠ `at` is the SERVER's clock, not the phone's. A beat is evidence that this app
    reached the fleet just now, and a phone with a wrong clock would otherwise be
    able to report itself permanently fresh or permanently stale. The phone's own
    times are kept only where they say something about the phone (`startedAt`).

    Best-effort status, never control: an unparseable `startedAt` is dropped rather
    than refused, because an app on an older build should cost its own detail and
    nothing else — least of all the beat itself, which is the part that matters.
    """
    now = datetime.now(UTC)
    store = _store()
    try:
        record_beat(
            store,
            Beat(
                device=body.device,
                app=body.app,
                version=body.version,
                started_at=_iso_or_none(body.startedAt),
                streaming=body.streaming,
                charging=body.charging,
                mic_ok=body.micOk,
                at=now,
            ),
        )
    finally:
        store.close()
    return {"ok": True}


@app.get("/api/devices/heartbeat")
def heartbeats() -> HeartbeatsOut:
    """Every mic app's last beat, for the fleetwatch collector that grades it.

    No verdict here, for the same reason as the outbox: what counts as too long
    belongs beside the other fleetwatch thresholds rather than split across two
    repositories.
    """
    store = _store()
    try:
        beats = read_beats(store)
    finally:
        store.close()
    return {"items": [_heartbeat(b) for b in beats]}


def _heartbeat(beat: Beat) -> HeartbeatOut:
    return {
        "device": beat.device,
        "app": beat.app,
        "version": beat.version,
        "startedAt": beat.started_at.isoformat() if beat.started_at else None,
        "streaming": beat.streaming,
        "charging": beat.charging,
        "micOk": beat.mic_ok,
        "at": beat.at.isoformat(),
    }


@app.get("/api/devices/outbox")
def outboxes() -> OutboxesOut:
    """Every phone's last outbox report, for the fleetwatch collector that grades it.

    No verdict here on purpose. What counts as too long belongs with the other
    fleetwatch thresholds, beside the checks it will sit next to, rather than
    split across two repositories.
    """
    store = _store()
    try:
        reports = read_reports(store)
    finally:
        store.close()
    return {
        "items": [
            {
                "device": r.device,
                "queued": r.queued,
                "oldestQueuedAt": (
                    r.oldest_queued_at.isoformat() if r.oldest_queued_at else None
                ),
                "failing": r.failing,
                "reason": r.reason,
                "at": r.at.isoformat(),
            }
            for r in reports
        ]
    }


def _iso_or_none(value: str | None) -> datetime | None:
    if not value:
        return None
    try:
        return datetime.fromisoformat(value).astimezone(UTC)
    except ValueError:
        return None


def _with_token(state: CaptureOut) -> CaptureOut:
    """Stamp the state's fingerprint (CaptureOut.stateToken): the value a long-poll
    echoes back as ?known= so "unchanged" is the server's judgement, not the
    client's field-by-field comparison."""
    payload = {k: v for k, v in state.items() if k != "stateToken"}
    digest = hashlib.sha256(json.dumps(payload, sort_keys=True).encode())
    state["stateToken"] = digest.hexdigest()[:12]
    return state


def _capture_state(running: bool) -> CaptureOut:
    """Local (capturing-host) view: the pause file IS the actuation, so desired and
    confirmed are the same thing and the state is settled by construction."""
    until = capture_control.paused_until(DATA_ROOT)
    iso = until.isoformat() if until else None
    return _with_token(
        {
            "running": running,
            "pausedUntil": iso,
            "desiredRunning": running,
            "desiredPausedUntil": iso,
            "settled": True,
            "micReachable": True,
            "stateToken": "",
        }
    )


def _fleet_capture_state(store: Store, now: datetime) -> CaptureOut:
    """The fleet holds the *desired* state (intent) while the Mac actuates and
    reports back, so the two can disagree for a couple of mirror cycles. Serve both:
    running/pausedUntil carry the mic's confirmed word (falling back to desired when
    it isn't reporting), desired* carries the intent, and settled says whether they
    agree — the client renders the disagreement as "Pausing…"/"Resuming…" instead of
    flapping between two truths it can't tell apart."""
    until = capture_control.intent_until(store, now)
    desired_running = until is None
    desired_until = until.isoformat() if until else None
    reported = capture_control.reported_state(store, now)
    if reported is None:
        return _with_token(
            {
                "running": desired_running,
                "pausedUntil": desired_until,
                "desiredRunning": desired_running,
                "desiredPausedUntil": desired_until,
                "settled": False,
                "micReachable": False,
                "stateToken": "",
            }
        )
    # Settled = the mic confirmed the desired state. When paused, the resume-by must
    # match too, so extending a pause (snooze) reads as transitioning until applied;
    # the Mac round-trips the intent's exact ISO string, so equality is exact.
    settled = reported.running == desired_running and (
        desired_running or reported.paused_until == desired_until
    )
    return _with_token(
        {
            "running": reported.running,
            "pausedUntil": reported.paused_until,
            "desiredRunning": desired_running,
            "desiredPausedUntil": desired_until,
            "settled": settled,
            "micReachable": True,
            "stateToken": "",
        }
    )


# Long-poll bounds: never hold a request past the cap (proxies and threadpools need
# a horizon), and re-derive the state every slice while hanging — transitions with no
# notify (a pause elapsing, a report aging into micReachable=False, a break-glass CLI
# pause writing the file directly) surface within a slice.
_WAIT_CAP_S = 25.0
_WAIT_SLICE_S = 2.0


def _capture_snapshot() -> CaptureOut:
    now = datetime.now(UTC)
    if capture_control.is_fleet():
        store = _store()
        try:
            return _fleet_capture_state(store, now)
        finally:
            store.close()
    running = capture_control.capture_running() and not capture_control.is_paused(
        DATA_ROOT, now
    )
    return _capture_state(running)


@app.get("/api/capture")
def capture_status(wait: float = 0, known: str = "") -> CaptureOut:
    """Whether the always-on capture is recording, and (if paused) when it
    auto-resumes by. Agents self-gate, so they stay loaded while paused — "running"
    means the capture agent is loaded *and* not currently paused. On the fleet (Isis)
    there is no local agent: it reports the Mac's mirrored state instead.

    Long-poll: with ?wait=<seconds>&known=<stateToken>, the request hangs while the
    state still fingerprints to `known` — a press or a confirming mirror report
    wakes it in ~RTT (capture_control.notify_capture_changed) instead of a poll
    interval. Without the params (an older client) it answers immediately."""
    # The version is read BEFORE each snapshot: a change landing mid-snapshot
    # makes the next wait return at once instead of being lost to the gap.
    seen = capture_control.capture_change_version()
    state = _capture_snapshot()
    deadline = time.monotonic() + min(wait, _WAIT_CAP_S)
    while state["stateToken"] == known:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            break
        capture_control.wait_capture_changed(min(_WAIT_SLICE_S, remaining), seen=seen)
        seen = capture_control.capture_change_version()
        state = _capture_snapshot()
    return state


@app.post("/api/capture/pause")
def capture_pause() -> CaptureOut:
    """Stop capture so the room can be worked in without recording. Bounded: it
    auto-resumes by the returned time even if left. On the fleet this records *intent*
    the Mac mirrors onto the mic; on the Mac it writes the local pause file directly."""
    now = datetime.now(UTC)
    if capture_control.is_fleet():
        store = _store()
        try:
            capture_control.intent_pause(store, now)
            state = _fleet_capture_state(store, now)
        finally:
            store.close()
        _log.info("PAUSE intent recorded (fleet)")
        # Wake the hanging mirror exchange (intent changed) and every hanging
        # client poll — the press propagates in ~RTT, not a poll interval.
        capture_control.notify_capture_changed()
        # Desired just flipped; confirmed lags until the Mac applies — the client
        # shows "Pausing…", and the next poll returns this same shape (no flap).
        return state
    capture_control.pause(DATA_ROOT, now)
    _log.info("PAUSE requested")
    capture_control.notify_capture_changed()
    return _capture_state(running=False)


@app.post("/api/capture/resume")
def capture_resume() -> CaptureOut:
    """Start capture again now."""
    if capture_control.is_fleet():
        store = _store()
        try:
            capture_control.intent_resume(store)
            state = _fleet_capture_state(store, datetime.now(UTC))
        finally:
            store.close()
        _log.info("RESUME intent recorded (fleet)")
        capture_control.notify_capture_changed()
        return state
    capture_control.resume(DATA_ROOT)
    _log.info("RESUME requested")
    capture_control.notify_capture_changed()
    return _capture_state(running=True)


@app.get("/api/search")
def search(q: str, limit: int = 100) -> ItemsOut:
    store = _store()
    try:
        return {"items": [_transcript(s) for s in store.search(q, limit=limit)]}
    finally:
        store.close()


@app.get("/api/timeline")
def timeline(limit: int = 200, before: str | None = None) -> PageOut:
    try:
        cursor = _parse_iso(before)
    except ValueError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc
    store = _store()
    try:
        rows = store.recent_transcripts(limit=limit, before=cursor)
        # Newest-first from the DB; reverse so the page reads top-to-bottom in
        # conversation order. `hasMore` drives the "load older" cursor. >=, not ==:
        # a page extends past `limit` when its boundary has same-instant ties.
        items = [_transcript(s) for s in reversed(rows)]
        return {"items": items, "hasMore": len(rows) >= limit}
    finally:
        store.close()


_PREVIEW_MIN_CONFIDENCE = 0.5


def _conversation(conv: Conversation[TranscriptSegment]) -> ConversationOut:
    # Preview with the first reasonably-confident line, so the card isn't headed
    # by a low-confidence guess; fall back to the first turn if none qualifies.
    preview = next(
        (
            turn.text
            for turn in conv.turns
            if turn.text.strip()
            and (turn.asr_confidence or 0.0) >= _PREVIEW_MIN_CONFIDENCE
        ),
        conv.turns[0].text,
    )
    return {
        "start": conv.start.isoformat(),
        "end": conv.end.isoformat(),
        "turnCount": conv.turn_count,
        "speakers": list(conv.speakers),
        "preview": preview,
        "moments": [_moment(moment) for moment in cluster_moments(conv.turns)],
    }


def _moment(moment: Moment[TranscriptSegment]) -> MomentOut:
    # Borrow the strongest co-located guess onto each spine turn (same speech, other
    # mics): the spine is the cleanest *transcription*, but the best *attribution* may
    # live on another mic's version — show that, not the spine mic's weaker guess.
    guesses = best_colocated_guess(moment.primary, moment.alternates)
    return {
        "start": moment.start.isoformat(),
        "end": moment.end.isoformat(),
        "primary": [
            _transcript(turn, guess=guesses.get(turn.id)) for turn in moment.primary
        ],
        "alternates": [_transcript(turn) for turn in moment.alternates],
        "sources": list(moment.sources),
    }


@app.get("/api/conversations")
def conversations(
    limit: int = 200,
    before: str | None = None,
    after: str | None = None,
    gap: float = DEFAULT_GAP_SECONDS,
    source: str | None = None,
) -> ConversationsOut:
    """Recent turns grouped into conversations by silence gaps (`gap` seconds).

    Page back with `before` (the oldest `start` seen) or forward with `after` (the
    newest `end` seen). `hasMore` means more exists in the direction requested. The
    conversation at the page edge may be truncated. `source` restricts to one
    recorder/session (how the sessions view focuses on a single meeting).
    """
    try:
        before_cur = _parse_iso(before)
        after_cur = _parse_iso(after)
    except ValueError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc
    store = _store()
    try:
        rows = store.recent_transcripts(
            limit=limit, before=before_cur, after=after_cur, source=source
        )
        # `after` rows already come oldest-first (conversation order); otherwise the
        # newest-first rows are reversed into chronological order.
        ordered = rows if after_cur is not None else list(reversed(rows))
        convs = segment_conversations(ordered, gap_seconds=gap)
        return {
            "items": [_conversation(conv) for conv in convs],
            # >=, not ==: a page extends past `limit` on same-instant boundary ties.
            "hasMore": len(rows) >= limit,
        }
    finally:
        store.close()


@app.get("/api/transcripts")
def transcripts(ids: str) -> ItemsOut:
    """Fetch specific turns by id (comma-separated), in the requested order.

    Backs deep links to a hand-picked set of fragments for listening/correcting.
    """
    try:
        wanted = [int(piece) for piece in ids.split(",") if piece.strip()]
    except ValueError as exc:
        raise HTTPException(status_code=400, detail="ids must be integers") from exc
    store = _store()
    try:
        # Resolve to the live version so a corrected/reprocessed fragment shows
        # its current text, not the stale original the link pointed at. Dedupe
        # in case several requested ids now resolve to the same turn.
        items: list[TranscriptOut] = []
        seen: set[int] = set()
        for tid in wanted:
            seg = store.current_version(tid)
            if seg is not None and seg.id not in seen:
                seen.add(seg.id)
                items.append(_transcript(seg))
        return {"items": items}
    finally:
        store.close()


@app.get("/api/review")
def review(limit: int = 50) -> ItemsOut:
    store = _store()
    try:
        return {"items": [_transcript(s) for s in review_queue(store, limit=limit)]}
    finally:
        store.close()


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


@app.get("/api/train")
def train(
    limit: int = 40,
    since: str | None = None,
    until: str | None = None,
    order: str = "loudness",
) -> TrainOut:
    """The labeling queue, scoped by `since`/`until` (ISO).

    `order` chooses how turns are ordered: "loudness" (loud/clear first, TV/film
    deprioritized — the labeling default) or "time" (oldest first, to read a
    conversation in sequence). `corrections` is the labelled-corpus size (progress).
    """
    try:
        since_cur = _parse_iso(since)
        until_cur = _parse_iso(until)
    except ValueError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc
    store = _store()
    try:
        if order == "time":
            turns = store.training_queue(
                min_confidence=_TRAIN_MIN_CONFIDENCE,
                max_confidence=_TRAIN_MAX_CONFIDENCE,
                limit=limit,
                since=since_cur,
                until=until_cur,
                order="time",
            )
            return {
                "items": [_transcript(s) for s in turns],
                "corrections": store.correction_count(),
                "bySpeaker": store.corrections_by_speaker(),
            }

        # "Best first": rank candidates by training value — clear + substantial +
        # novel, with TV/film pushed below household speech. The clearest turns
        # (precomputed loudness, filled offline by the worker) form the candidate
        # pool; the value score then promotes longer, not-yet-labelled speech over
        # loud one-word fillers, so each label teaches the model as much as
        # possible. Stays a cheap read + sort.
        candidates = store.training_queue(
            min_confidence=_TRAIN_MIN_CONFIDENCE,
            max_confidence=_TRAIN_MAX_CONFIDENCE,
            limit=_TRAIN_CANDIDATES,
            since=since_cur,
            until=until_cur,
            order="loudness",
        )
        spans = store.media_spans(
            max_gap_s=_MEDIA_MAX_GAP_S, min_duration_s=_MEDIA_MIN_DURATION_S
        )
        labelled = store.corrected_texts()

        def in_media(s: TranscriptSegment) -> bool:
            return any(start <= s.start < end for start, end in spans)

        def value(s: TranscriptSegment) -> float:
            return training_value(
                loudness=s.loudness or 0.0,
                duration_s=(s.end - s.start).total_seconds(),
                repeat=normalize_text(s.text) in labelled,
                diversity=diversity_factor(s.text),
                foreign=foreign_script_ratio(s.text),
            )

        scored = [(s, in_media(s), value(s)) for s in candidates]
        scored.sort(key=lambda t: (t[1], -t[2]))
        return {
            "items": [_transcript(s) for s, _, _ in scored[:limit]],
            "corrections": store.correction_count(),
            "bySpeaker": store.corrections_by_speaker(),
        }
    finally:
        store.close()


@app.post("/api/unintelligible")
def unintelligible(body: UnintelligibleIn) -> OkOut:
    """Mark a turn humanly unintelligible: drop it from the queue/timeline (kept,
    recoverable) and out of the training corpus — its real fix is better capture.
    """
    store = _store()
    try:
        store.hide(body.id, CANT_MAKE_OUT_REASON)
        return {"ok": True}
    finally:
        store.close()


@app.post("/api/unhide")
def unhide(body: UnhideIn) -> OkOut:
    store = _store()
    try:
        store.unhide(body.id)
        return {"ok": True}
    finally:
        store.close()


def _scan_job() -> ScanJob:
    """The one scan job, made lazily so it binds to DATA_ROOT as the tests patch it."""
    global _SCAN_JOB  # noqa: PLW0603 - one job per process, by design
    if _SCAN_JOB is None:
        _SCAN_JOB = ScanJob(_store)
    return _SCAN_JOB


@app.post("/api/quiet/scan")
def quiet_scan_start() -> QuietScanOut:
    """Start measuring the archive in the background, and report where it's got to.

    The work outlives the request (~20 minutes of ffmpeg), so this returns at once.
    Calling it while a scan runs joins that one rather than starting a second. Poll GET
    for progress; the scan is durable, so closing the page is harmless.
    """
    job = _scan_job()
    job.start()
    return _scan_progress(job)


@app.get("/api/quiet/scan")
def quiet_scan_progress() -> QuietScanOut:
    """How far measuring the archive has got, and whether it is still running."""
    return _scan_progress(_scan_job())


@app.post("/api/quiet/scan/stop")
def quiet_scan_stop() -> QuietScanOut:
    """Stop the scan after the file it's on. Everything measured stays measured, so this
    pauses rather than discards — starting again resumes."""
    job = _scan_job()
    job.stop()
    return _scan_progress(job)


def _scan_progress(job: ScanJob) -> QuietScanOut:
    progress = job.progress()
    return {
        "running": progress.running,
        "measured": progress.measured,
        "total": progress.total,
        "analysed": progress.analysed,
        "toAnalyse": progress.to_analyse,
    }


@app.get("/api/quiet/spans")
def quiet_spans_list(min_seconds: int = 300) -> QuietSpansOut:
    """The long total-quiet spans, biggest first — see `recall.quiet.rank_spans`.

    Each carries what the microphone heard that was *not* speech (`soundSeconds`, its
    loudest moment, and how far that rose above this mic's own floor), so the review can
    show a span's bumps and coughs rather than a bare number of minutes.
    """
    from recall.calibrate import event_threshold  # noqa: PLC0415
    from recall.envelope import summarize_sound  # noqa: PLC0415
    from recall.quiet import quiet_spans, rank_spans  # noqa: PLC0415

    store = _store()
    try:
        spans = quiet_spans(store, min_duration_s=float(min_seconds))
        measured = []
        for span in spans:
            envelopes = store.audio_envelopes(list(span.audio_ids))
            sound = summarize_sound(
                [envelopes[a] for a in span.audio_ids if a in envelopes],
                event_threshold(store, span.source_id),
                structure=store.span_structure(list(span.audio_ids)),
            )
            measured.append((span, sound))
    finally:
        store.close()

    measured = rank_spans(measured)
    return {
        "items": [
            {
                "source": span.source_id,
                "start": span.start.isoformat(),
                "end": span.end.isoformat(),
                "durationS": span.duration_s,
                "audioIds": [int(a) for a in span.audio_ids],
                "soundSeconds": sound.sound_seconds,
                "loudestDb": sound.loudest_db,
                "marginDb": sound.margin_db,
                "silent": sound.silent,
                "structure": sound.structure,
            }
            for span, sound in measured
        ]
    }


@app.get("/api/quiet/envelope")
def quiet_envelope(
    source: str, start: str, end: str, max_points: int = 1500
) -> EnvelopeOut:
    """The waveform of one source over [start, end) — what the review draws to judge a
    span: whether it really is dead air throughout, and what broke the quiet at its
    edges. Ask for a window wider than the span to see the sounds that ended it."""
    from recall.envelope import (  # noqa: PLC0415 - keeps ffmpeg/numpy use local
        EnvelopeSegment,
        build_envelope,
    )

    try:
        window_start = _require_time(start)
        window_end = _require_time(end)
    except ValueError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc
    if window_end <= window_start:
        raise HTTPException(status_code=400, detail="end must be after start")

    from recall.calibrate import event_threshold  # noqa: PLC0415
    from recall.envelope import (  # noqa: PLC0415
        decode_envelope,
        segment_envelope,
    )

    store = _store()
    try:
        rows = store.audio_segments_between(source, window_start, window_end)
        stored = store.audio_envelopes([row[0] for row in rows])
        # What counts as a sound is a property of *this* microphone, measured from it.
        threshold = event_threshold(store, source)
    finally:
        store.close()

    # Read the shape the scan already decoded. Only a segment the scan has never
    # examined falls back to ffmpeg — otherwise opening a 100-minute span would decode
    # its 130 files on the spot, every time. Membership, not truthiness: a segment the
    # scan found undecodable is stored as an *empty* envelope, and that is an answer
    # (draw a gap), not a cache miss to retry against a file that will never decode.
    by_path = {
        row[1]: decode_envelope(stored[row[0]]) for row in rows if row[0] in stored
    }
    envelope = build_envelope(
        [EnvelopeSegment(*row) for row in rows],
        start=window_start,
        end=window_end,
        threshold_db=threshold,
        max_points=max_points,
        envelope_of=lambda path: (
            by_path[path] if path in by_path else segment_envelope(path)
        ),
    )
    return {
        "start": envelope.start.isoformat(),
        "end": envelope.end.isoformat(),
        "bucketS": envelope.bucket_s,
        "thresholdDb": threshold,
        "points": list(envelope.points),
        "segments": [
            {
                "audioId": int(s.audio_id),
                "start": s.start.isoformat(),
                "end": s.end.isoformat(),
                "meanDb": s.mean_db,
            }
            for s in envelope.segments
        ],
        "events": [
            {
                "start": e.start.isoformat(),
                "end": e.end.isoformat(),
                "peakDb": e.peak_db,
            }
            for e in envelope.events
        ],
    }


@app.get("/api/quiet/audio/{audio_id}")
def quiet_audio(audio_id: int) -> Response:
    """Stream one capture segment's raw audio, so a span can be played to confirm it's
    quiet before deleting it."""
    store = _store()
    try:
        segment = store.audio_segment(AudioSegmentId(audio_id))
    finally:
        store.close()
    if segment is None or not Path(segment.path).exists():
        raise HTTPException(status_code=404, detail="segment not found")
    return FileResponse(segment.path, media_type="audio/ogg")


@app.post("/api/quiet/delete")
def quiet_delete(body: QuietDeleteIn) -> QuietDeletedOut:
    """Hard-delete a confirmed quiet span: its capture segments and everything derived,
    plus the Opus files on disk. Reports how many segments went and the bytes freed.

    Logged, before and after. This is the one operation in the app that destroys data
    the household cannot get back, and until now it left no record of itself: when two
    deletes were fired at once, what they had actually taken could only be reconstructed
    by diffing the database against an old snapshot. An irreversible act should say what
    it did.

    Deleting the same segments twice is not an error — a duplicate request simply finds
    the rows gone and removes nothing. The count says so.
    """
    store = _store()
    try:
        span = store.audio_segment_bounds([AudioSegmentId(i) for i in body.audioIds])
        if span is not None:
            source, start, end = span
            _log.info(
                "DELETE requested: %d segments, %s, %s -> %s",
                len(body.audioIds),
                source,
                start.isoformat(),
                end.isoformat(),
            )
        paths = store.delete_audio_segments([AudioSegmentId(i) for i in body.audioIds])
    finally:
        store.close()
    freed = 0
    for path in paths:
        file = Path(path)
        try:
            freed += file.stat().st_size
            file.unlink()
        except OSError:
            continue
    _log.info(
        "DELETE done: %d segments removed, %.1f MB freed (%d requested)",
        len(paths),
        freed / 1e6,
        len(body.audioIds),
    )
    return {"deleted": len(paths), "freedBytes": freed}


@app.post("/api/correct")
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


@app.post("/api/sessions/{source}/voice")
def name_voice(source: str, body: VoiceNameIn) -> OkOut:
    """Human-name a diarization voice across a session — labels every turn of that
    voice at once. Writes no correction, but the label feeds the voiceprint backfill
    (`speaker_label` is its work-list), so the named voice becomes matchable — a
    meeting's clinician is enrolled like any household voice."""
    name = (body.name or "").strip() or None
    store = _store()
    try:
        store.name_voice(source, body.cluster, name)
    finally:
        store.close()
    return {"ok": True}


@app.post("/api/turn/{segment_id}/speaker")
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


@app.post("/api/turn/{segment_id}/nudge")
def turn_nudge(segment_id: int, body: NudgeIn) -> OkOut:
    """Move one edge of a turn by ear — hand-tune a split boundary when the aligner's
    cut is slightly off, so the bubble plays exactly its words."""
    store = _store()
    try:
        store.nudge_turn(segment_id, body.edge, body.delta)
    finally:
        store.close()
    return {"ok": True}


@app.post("/api/refine")
def refine_request(body: RefineRequestIn) -> OkOut:
    """Queue an on-demand diarize-refine of [start, end) of a recording. The idle-gated
    refine daemon runs it, so the heavy pass stays off live capture — the timeline's
    'Refine this' action."""
    try:
        start = _require_time(body.start)
        end = _require_time(body.end)
    except ValueError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc
    store = _store()
    try:
        store.add_refine_request(body.source, start, end)
    finally:
        store.close()
    return {"ok": True}


@app.post("/api/ab-compare")
def ab_compare_start(body: AbCompareStartIn) -> NewIdOut:
    """Queue a non-destructive A/B comparison of two ASR models over a recording. The
    refine daemon runs it; poll `GET /api/ab-compare/{id}` for the result."""
    try:
        frm = _parse_iso(body.frm or None)
        to = _parse_iso(body.to or None)
    except ValueError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc
    store = _store()
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
    store = _store()
    try:
        return {"items": [_ab_run_summary(j) for j in store.list_ab_compare_runs()]}
    finally:
        store.close()


@app.get("/api/ab-compare/{run_id}")
def ab_compare_run(run_id: int) -> AbCompareRunOut:
    """One run in full: its summary plus the per-span WER evidence (each with the
    audio of that span) and the whole-segment text diffs. Lists are empty until done."""
    store = _store()
    try:
        job = store.get_ab_compare_run(run_id)
    finally:
        store.close()
    if job is None:
        raise HTTPException(status_code=404, detail="no such run")
    scores, diffs = _ab_run_detail(job)
    return {"summary": _ab_run_summary(job), "scores": scores, "segmentDiffs": diffs}


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


@app.post("/api/sessions/{source}/assign")
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


@app.get("/api/sessions/{source}/voices")
def voice_suggestions(source: str) -> VoiceSuggestionsOut:
    """Auto-suggested name per diarization voice in a session, from cached voiceprint
    guesses — so an enrolled household member is identified for you (the clinician you
    name by hand). `{cluster: name}`, only the confident, unambiguous ones."""
    store = _store()
    try:
        return {"suggestions": store.session_voice_suggestions(source)}
    finally:
        store.close()


@app.get("/api/speakers")
def speakers() -> SpeakerNamesOut:
    """Known speaker names (enrolled voices + assigned labels) for autocompleting the
    voice naming, so the same person is spelled the same across sessions."""
    store = _store()
    try:
        return {"names": store.known_speaker_names()}
    finally:
        store.close()


def _label(f: LabelledFragment) -> LabelOut:
    return {
        "id": f.correction_id,
        "text": f.text,
        "speaker": f.speaker,
        "language": f.language,
        "start": f.start.isoformat(),
        "audioUrl": f"/api/correction/{f.correction_id}/audio",
    }


@app.get("/api/corrections")
def corrections(speaker: str | None = None, limit: int = 200) -> CorrectionsOut:
    """The labelled fragments for review/audit, newest first, optionally one voice."""
    store = _store()
    try:
        items = store.list_corrections(speaker=speaker, limit=limit)
        return {
            "items": [_label(f) for f in items],
            "bySpeaker": store.corrections_by_speaker(),
        }
    finally:
        store.close()


@app.get("/api/correction/{correction_id}/audio")
def correction_audio(correction_id: int, context: bool = False) -> Response:
    """The labelled clip's audio. By default plays the *exact* trimmed span (to
    audit the cut); `context=true` adds the usual lead-in/-out for easy listening.
    """
    store = _store()
    try:
        frag = store.get_correction(correction_id)
        if frag is None or frag.audio_segment_id is None:
            raise HTTPException(status_code=404, detail="no audio")
        ref = store.audio_segment_ref(frag.audio_segment_id)
        if ref is None:
            raise HTTPException(status_code=404, detail="no audio")
        path, audio_start = ref
        raw_start = (frag.start - audio_start).total_seconds()
        raw_end = (frag.end - audio_start).total_seconds()
        if context:
            rel_start, rel_end = clip_window(
                raw_start, raw_end, pad=_AUDIO_PAD_S, minimum=_AUDIO_MIN_S
            )
        else:
            rel_start, rel_end = max(0.0, raw_start), raw_end
        with tempfile.TemporaryDirectory() as tmp:
            clip = Path(tmp) / "clip.wav"
            slice_clip(Path(path), clip, rel_start, rel_end)
            norm = Path(tmp) / "clip-norm.wav"
            normalize_loudness(clip, norm)
            return Response(content=norm.read_bytes(), media_type="audio/wav")
    finally:
        store.close()


@app.post("/api/correction/{correction_id}/speaker")
def correction_reassign(correction_id: int, body: ReassignIn) -> OkOut:
    """Fix a mis-tagged label's voice (and its voiceprint + timeline segment)."""
    store = _store()
    try:
        store.set_correction_speaker(correction_id, body.speaker)
        return {"ok": True}
    finally:
        store.close()


@app.post("/api/correction/{correction_id}/nudge")
def correction_nudge(correction_id: int, body: NudgeIn) -> OkOut:
    """Move one boundary of a label (fix a cut that's too tight or too loose)."""
    store = _store()
    try:
        store.nudge_correction(correction_id, body.edge, body.delta)
        return {"ok": True}
    finally:
        store.close()


@app.post("/api/correction/{correction_id}/hide")
def correction_hide(correction_id: int) -> OkOut:
    """Soft-remove a bad label from the corpus, counts, and its voiceprint."""
    store = _store()
    try:
        store.hide_correction(correction_id, "review")
        return {"ok": True}
    finally:
        store.close()


@app.post("/api/split")
def split(body: SplitIn) -> NewIdsOut:
    """Replace one turn with several single-speaker fragments (per-speaker labels)."""
    store = _store()
    try:
        # _parse_iso raises ValueError on malformed input (-> the 400 below); it
        # returns None only for a missing value, which is equally a caller error —
        # NEVER substitute a made-up time: a wrong span would slice wrong audio
        # into the fine-tune corpus.
        frags = [
            SpeakerFragment(
                start=_require_time(f.start),
                end=_require_time(f.end),
                text=f.text,
                speaker=f.speaker,
            )
            for f in body.fragments
        ]
        new_ids = split_correction(store, body.id, frags, now=datetime.now(UTC))
        return {"newIds": new_ids}
    except ValueError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc
    finally:
        store.close()


@app.get("/api/around/{transcript_id}")
def around(transcript_id: int, n: int = 2) -> AroundOut:
    """The `n` current turns just before and after one — context for labeling."""
    store = _store()
    try:
        target = store.get_transcript(transcript_id)
        if target is None:
            raise HTTPException(status_code=404, detail="no such turn")
        window = timedelta(minutes=2)
        nearby = store.segments_in_range(target.start - window, target.end + window)
        idx = next((i for i, s in enumerate(nearby) if s.id == transcript_id), None)
        if idx is None:
            return {"before": [], "after": []}
        return {
            "before": [_transcript(s) for s in nearby[max(0, idx - n) : idx]],
            "after": [_transcript(s) for s in nearby[idx + 1 : idx + 1 + n]],
        }
    finally:
        store.close()


@app.get("/api/audio/{transcript_id}")
def audio(transcript_id: int) -> Response:
    store = _store()
    try:
        segment = store.get_transcript(transcript_id)
        if segment is None or segment.audio_segment_id is None:
            raise HTTPException(status_code=404, detail="no audio")
        ref = store.audio_segment_ref(segment.audio_segment_id)
        if ref is None:
            raise HTTPException(status_code=404, detail="no audio")
        path, audio_start = ref
        phrase_start = (segment.start - audio_start).total_seconds()
        phrase_end = (segment.end - audio_start).total_seconds()
        if _precise(segment):
            rel_start, rel_end = clip_window(
                phrase_start, phrase_end, pad=_AUDIO_TIGHT_PAD_S, minimum=0.0
            )
        else:
            rel_start, rel_end = clip_window(
                phrase_start, phrase_end, pad=_AUDIO_PAD_S, minimum=_AUDIO_MIN_S
            )
        with tempfile.TemporaryDirectory() as tmp:
            clip = Path(tmp) / "clip.wav"
            slice_clip(Path(path), clip, rel_start, rel_end)
            # Uniform-gain loudness so it's audible without cranking the volume;
            # the raw recording is untouched (only the playback clip is shaped).
            norm = Path(tmp) / "clip-norm.wav"
            normalize_loudness(clip, norm)
            data = norm.read_bytes()
        return Response(content=data, media_type="audio/wav")
    finally:
        store.close()


@app.get("/api/audio-span")
def audio_span(from_id: int, to_id: int) -> Response:
    """One continuous clip for a joined bubble: the audio from the start of turn
    `from_id` to the end of turn `to_id`. A bubble is consecutive same-speaker turns
    sharing the recording's audio, so this is that speaker's uninterrupted stretch —
    played tight (the turns are word-snapped). 400 if the two turns aren't in the same
    recording (a session split into fragments); the UI falls back to per-turn play."""
    store = _store()
    try:
        first = store.get_transcript(from_id)
        last = store.get_transcript(to_id)
        if first is None or last is None or first.audio_segment_id is None:
            raise HTTPException(status_code=404, detail="no audio")
        if first.audio_segment_id != last.audio_segment_id:
            raise HTTPException(status_code=400, detail="span crosses recordings")
        ref = store.audio_segment_ref(first.audio_segment_id)
        if ref is None:
            raise HTTPException(status_code=404, detail="no audio")
        path, audio_start = ref
        span_start = (first.start - audio_start).total_seconds()
        span_end = (last.end - audio_start).total_seconds()
        rel_start, rel_end = clip_window(
            span_start, span_end, pad=_AUDIO_TIGHT_PAD_S, minimum=0.0
        )
        with tempfile.TemporaryDirectory() as tmp:
            clip = Path(tmp) / "clip.wav"
            slice_clip(Path(path), clip, rel_start, rel_end)
            norm = Path(tmp) / "clip-norm.wav"
            normalize_loudness(clip, norm)
            return Response(content=norm.read_bytes(), media_type="audio/wav")
    finally:
        store.close()


@app.get("/api/clip/{transcript_id}")
def clip(transcript_id: int, lead: float = 1.5, tail: float = 1.5) -> Response:
    """A turn's audio with `lead`/`tail` seconds of context — for the trimmer.

    The `X-Lead` header gives the actual seconds of lead included (clamped at the
    file start), so the UI can map a position in this clip back to absolute time.
    """
    store = _store()
    try:
        segment = store.get_transcript(transcript_id)
        if segment is None or segment.audio_segment_id is None:
            raise HTTPException(status_code=404, detail="no audio")
        ref = store.audio_segment_ref(segment.audio_segment_id)
        if ref is None:
            raise HTTPException(status_code=404, detail="no audio")
        path, audio_start = ref
        turn_start = (segment.start - audio_start).total_seconds()
        turn_end = (segment.end - audio_start).total_seconds()
        win_start = max(0.0, turn_start - lead)
        actual_lead = turn_start - win_start
        with tempfile.TemporaryDirectory() as tmp:
            raw = Path(tmp) / "clip.wav"
            out = Path(tmp) / "clip-norm.wav"
            slice_clip(Path(path), raw, win_start, turn_end + tail)
            normalize_loudness(raw, out)
            data = out.read_bytes()
        return Response(
            content=data,
            media_type="audio/wav",
            headers={"X-Lead": f"{actual_lead:.3f}"},
        )
    finally:
        store.close()


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


@app.get("/api/suggest/{segment_id}")
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
