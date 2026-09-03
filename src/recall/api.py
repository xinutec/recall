"""FastAPI JSON API over the recall core (consumed by the Angular front-end).

A thin transport layer: every endpoint calls the typed core (store / review /
speakerid) and returns JSON. FastAPI/pydantic are waived in mypy (present only in
the venv), so keep logic in the core — not here. When a built Angular app exists
at frontend/dist/, it's served as static files so everything is one origin.
"""

from __future__ import annotations

import json
import logging
import shutil
import subprocess
import tempfile
import threading
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import cast
from zoneinfo import ZoneInfo

from fastapi import FastAPI, File, Form, HTTPException, UploadFile
from fastapi.responses import FileResponse, Response

from recall import capture_control
from recall.api_capture import fleet_capture_state, register_capture_routes
from recall.api_client_reports import register_client_report_routes
from recall.api_devices import register_device_routes
from recall.api_models import (
    AbCompareStartIn,
    AskIn,
    AssignSpanIn,
    ContextIn,
    CorrectIn,
    NudgeIn,
    ReassignIn,
    RefineRequestIn,
    SessionRenameIn,
    SplitIn,
    TurnSpeakerIn,
    UnhideIn,
    UnintelligibleIn,
    VocabularyIn,
    VoiceNameIn,
)
from recall.api_quiet import register_quiet_routes
from recall.ask import build_ask_prompt, retrieve
from recall.asr import DEFAULT_MODEL, slice_clip
from recall.context import CONTEXT_KEY, household_context_block
from recall.conversation import assign_span
from recall.conversations import (
    DEFAULT_GAP_SECONDS,
    Conversation,
    segment_conversations,
)
from recall.finetune import DEFAULT_BASE_MODEL
from recall.llm import DEFAULT_LLM, Generator, make_generator
from recall.loudness import normalize_loudness
from recall.moments import Moment, best_colocated_guess, cluster_moments
from recall.paths import default_data_root
from recall.probe import probe_media
from recall.quality import foreign_script_ratio
from recall.ranking import (
    diversity_factor,
    normalize_text,
    training_value,
)
from recall.review import (
    SpeakerFragment,
    apply_correction,
    review_queue,
    split_correction,
)
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
    ContextOut,
    ConversationOut,
    ConversationsOut,
    CorrectionsOut,
    DaySummariesOut,
    ItemsOut,
    LabelOut,
    MomentOut,
    NewIdOut,
    NewIdsOut,
    OkOut,
    PageOut,
    SessionOut,
    SessionsOut,
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
from recall.sources import AudioSource, SourceKind
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


register_client_report_routes(app, client_log_path=_REPO / "logs" / "client.log")


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


register_capture_routes(app, store_factory=_store, data_root=lambda: DATA_ROOT)
register_device_routes(
    app,
    store_factory=_store,
    data_root=lambda: DATA_ROOT,
    fleet_capture_state=fleet_capture_state,
)


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


register_quiet_routes(app, store_factory=_store, require_time=_require_time)


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
