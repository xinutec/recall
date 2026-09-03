"""FastAPI JSON API over the recall core (consumed by the Angular front-end).

A thin transport layer: every endpoint calls the typed core (store / review /
speakerid) and returns JSON. FastAPI/pydantic are waived in mypy (present only in
the venv), so keep logic in the core — not here. When a built Angular app exists
at frontend/dist/, it's served as static files so everything is one origin.
"""

from __future__ import annotations

import logging
import tempfile
import threading
from datetime import UTC, datetime
from pathlib import Path

from fastapi import FastAPI, HTTPException
from fastapi.responses import FileResponse, Response

from recall import capture_control
from recall.api_capture import fleet_capture_state, register_capture_routes
from recall.api_client_reports import register_client_report_routes
from recall.api_devices import register_device_routes
from recall.api_experiments import register_experiment_routes
from recall.api_models import (
    AskIn,
    AssignSpanIn,
    ContextIn,
    CorrectIn,
    NudgeIn,
    ReassignIn,
    SplitIn,
    TurnSpeakerIn,
    UnhideIn,
    UnintelligibleIn,
    VocabularyIn,
)
from recall.api_quiet import register_quiet_routes
from recall.api_reads import _precise, register_read_routes
from recall.api_reads import transcript_out as _transcript
from recall.api_sessions import register_session_routes
from recall.ask import build_ask_prompt, retrieve
from recall.asr import slice_clip
from recall.context import CONTEXT_KEY, household_context_block
from recall.conversation import assign_span
from recall.llm import DEFAULT_LLM, Generator, make_generator
from recall.loudness import normalize_loudness
from recall.paths import default_data_root
from recall.quality import foreign_script_ratio
from recall.ranking import (
    diversity_factor,
    normalize_text,
    training_value,
)
from recall.review import (
    SpeakerFragment,
    apply_correction,
    split_correction,
)
from recall.schemas import (
    AskOut,
    AssignResultOut,
    ContextOut,
    CorrectionsOut,
    DaySummariesOut,
    LabelOut,
    NewIdOut,
    NewIdsOut,
    OkOut,
    SpeakerNamesOut,
    SuggestOut,
    TodaySummaryOut,
    TrainOut,
    TranscriptOut,
    VocabularyOut,
    VoiceSuggestionsOut,
)
from recall.store import (
    LabelledFragment,
    Store,
    TranscriptSegment,
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


register_experiment_routes(
    app,
    store_factory=_store,
    require_time=_require_time,
    parse_iso=_parse_iso,
)


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
