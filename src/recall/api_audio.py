"""The audio-serving HTTP surface: per-turn playback, bubble spans, and the
trimmer clip — plus `clip_window`, the padding rule they all share.

Slice 9 of api.py's decomposition (#1342). Module-level handlers (audio_span
is called directly by tests), registrar-set store factory.
"""

from __future__ import annotations

import tempfile
from collections.abc import Callable
from pathlib import Path

from fastapi import FastAPI, HTTPException
from fastapi.responses import Response

from recall.api_reads import _precise
from recall.asr import slice_clip
from recall.loudness import normalize_loudness
from recall.store import Store

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

_store_factory: Callable[[], Store] | None = None


def _store() -> Store:
    assert _store_factory is not None, "register_audio_routes was never called"
    return _store_factory()


def register_audio_routes(app: FastAPI, *, store_factory: Callable[[], Store]) -> None:
    """Mount the audio-serving surface."""
    global _store_factory  # noqa: PLW0603 - the registrar's one job
    _store_factory = store_factory
    app.get("/api/audio/{transcript_id}")(audio)
    app.get("/api/audio-span")(audio_span)
    app.get("/api/clip/{transcript_id}")(clip)


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
