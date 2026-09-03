"""The labelling HTTP surface: the train queue, corrections, per-turn edits,
span assignment, voices, split, and the "sounds like" hint.

Slice 8 of api.py's decomposition (#1342). Module-level handlers (train and
suggest are called directly by tests) with registrar-set module state; the
clip-window helper is injected because it still lives in api.py with the
audio-serving family.
"""

from __future__ import annotations

import tempfile
from collections.abc import Callable
from datetime import UTC, datetime
from pathlib import Path

from fastapi import FastAPI, HTTPException
from fastapi.responses import Response

from recall.api_models import (
    AssignSpanIn,
    CorrectIn,
    NudgeIn,
    ReassignIn,
    SplitIn,
    TurnSpeakerIn,
    UnhideIn,
    UnintelligibleIn,
)
from recall.api_reads import transcript_out
from recall.asr import slice_clip
from recall.conversation import assign_span
from recall.loudness import normalize_loudness
from recall.quality import foreign_script_ratio
from recall.ranking import diversity_factor, normalize_text, training_value
from recall.review import SpeakerFragment, apply_correction, split_correction
from recall.schemas import (
    AssignResultOut,
    CorrectionsOut,
    LabelOut,
    NewIdOut,
    NewIdsOut,
    OkOut,
    SpeakerNamesOut,
    SuggestOut,
    TrainOut,
    VoiceSuggestionsOut,
)
from recall.store import LabelledFragment, Store, TranscriptSegment

# Train pre-fills "sounds like X" only when the leading candidate's likelihood
# (softmax over the enrolled people) clears this — a confirmable hint, not a coin
# flip. The timeline still shows every guess with its %.
_SUGGEST_MIN_PROB = 0.4
# Playback context for a labelled clip (mirrors the audio family's pads).
_AUDIO_PAD_S = 1.5
_AUDIO_MIN_S = 5.0

_store_factory: Callable[[], Store] | None = None
_parse_iso_fn: Callable[[str | None], datetime | None] | None = None
_require_time_fn: Callable[[str | None], datetime] | None = None
_clip_window: Callable[..., tuple[float, float]] | None = None


def _store() -> Store:
    assert _store_factory is not None, "register_label_routes was never called"
    return _store_factory()


def _parse_iso(value: str | None) -> datetime | None:
    assert _parse_iso_fn is not None
    return _parse_iso_fn(value)


def _require_time(value: str | None) -> datetime:
    assert _require_time_fn is not None
    return _require_time_fn(value)


def clip_window(
    start: float, end: float, *, pad: float, minimum: float
) -> tuple[float, float]:
    assert _clip_window is not None
    return _clip_window(start, end, pad=pad, minimum=minimum)


def register_label_routes(
    app: FastAPI,
    *,
    store_factory: Callable[[], Store],
    parse_iso: Callable[[str | None], datetime | None],
    require_time: Callable[[str | None], datetime],
    clip_window_fn: Callable[..., tuple[float, float]],
) -> None:
    """Mount the labelling surface. Dependencies land in module state."""
    global _store_factory, _parse_iso_fn, _require_time_fn, _clip_window  # noqa: PLW0603
    _store_factory = store_factory
    _parse_iso_fn = parse_iso
    _require_time_fn = require_time
    _clip_window = clip_window_fn
    app.get("/api/train")(train)
    app.post("/api/unintelligible")(unintelligible)
    app.post("/api/unhide")(unhide)
    app.post("/api/correct")(correct)
    app.post("/api/turn/{segment_id}/speaker")(turn_speaker)
    app.post("/api/turn/{segment_id}/nudge")(turn_nudge)
    app.post("/api/sessions/{source}/assign")(assign)
    app.get("/api/sessions/{source}/voices")(voice_suggestions)
    app.get("/api/speakers")(speakers)
    app.get("/api/corrections")(corrections)
    app.get("/api/correction/{correction_id}/audio")(correction_audio)
    app.post("/api/correction/{correction_id}/speaker")(correction_reassign)
    app.post("/api/correction/{correction_id}/nudge")(correction_nudge)
    app.post("/api/correction/{correction_id}/hide")(correction_hide)
    app.post("/api/split")(split)
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
                "items": [transcript_out(s) for s in turns],
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
            "items": [transcript_out(s) for s, _, _ in scored[:limit]],
            "corrections": store.correction_count(),
            "bySpeaker": store.corrections_by_speaker(),
        }
    finally:
        store.close()


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


def unhide(body: UnhideIn) -> OkOut:
    store = _store()
    try:
        store.unhide(body.id)
        return {"ok": True}
    finally:
        store.close()


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


def turn_nudge(segment_id: int, body: NudgeIn) -> OkOut:
    """Move one edge of a turn by ear — hand-tune a split boundary when the aligner's
    cut is slightly off, so the bubble plays exactly its words."""
    store = _store()
    try:
        store.nudge_turn(segment_id, body.edge, body.delta)
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


def correction_reassign(correction_id: int, body: ReassignIn) -> OkOut:
    """Fix a mis-tagged label's voice (and its voiceprint + timeline segment)."""
    store = _store()
    try:
        store.set_correction_speaker(correction_id, body.speaker)
        return {"ok": True}
    finally:
        store.close()


def correction_nudge(correction_id: int, body: NudgeIn) -> OkOut:
    """Move one boundary of a label (fix a cut that's too tight or too loose)."""
    store = _store()
    try:
        store.nudge_correction(correction_id, body.edge, body.delta)
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
