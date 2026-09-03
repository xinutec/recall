"""The read surface: search, timeline, conversations, deep links, context —
and the turn serializer every family shares.

Slice 7 of api.py's decomposition (#1342). Handlers are module-level (the
timeline/conversations tests call them directly), with dependencies as
registrar-set module state — the api_capture pattern. `transcript_out` (né
api._transcript) is the shared serializer; the families still in api.py
import it back under its old private name until they move.
"""

from __future__ import annotations

from collections.abc import Callable
from datetime import datetime, timedelta

from fastapi import FastAPI, HTTPException

from recall.conversations import (
    DEFAULT_GAP_SECONDS,
    Conversation,
    segment_conversations,
)
from recall.moments import Moment, best_colocated_guess, cluster_moments
from recall.review import review_queue
from recall.schemas import (
    AroundOut,
    ConversationOut,
    ConversationsOut,
    ItemsOut,
    MomentOut,
    PageOut,
    Tier,
    TranscriptOut,
)
from recall.store import (
    DIARIZED_MARKER,
    HUMAN_MODEL,
    LIVE_MODEL,
    Store,
    TranscriptSegment,
)

_store_factory: Callable[[], Store] | None = None
_parse_iso_fn: Callable[[str | None], datetime | None] | None = None


def _store() -> Store:
    assert _store_factory is not None, "register_read_routes was never called"
    return _store_factory()


def _parse_iso(value: str | None) -> datetime | None:
    assert _parse_iso_fn is not None, "register_read_routes was never called"
    return _parse_iso_fn(value)


def register_read_routes(
    app: FastAPI,
    *,
    store_factory: Callable[[], Store],
    parse_iso: Callable[[str | None], datetime | None],
) -> None:
    """Mount the read surface. Dependencies land in module state (see header)."""
    global _store_factory, _parse_iso_fn  # noqa: PLW0603 - the registrar's one job
    _store_factory = store_factory
    _parse_iso_fn = parse_iso
    app.get("/api/search")(search)
    app.get("/api/timeline")(timeline)
    app.get("/api/conversations")(conversations)
    app.get("/api/transcripts")(transcripts)
    app.get("/api/review")(review)
    app.get("/api/around/{transcript_id}")(around)


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


def transcript_out(
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


def search(q: str, limit: int = 100) -> ItemsOut:
    store = _store()
    try:
        return {"items": [transcript_out(s) for s in store.search(q, limit=limit)]}
    finally:
        store.close()


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
        items = [transcript_out(s) for s in reversed(rows)]
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
            transcript_out(turn, guess=guesses.get(turn.id)) for turn in moment.primary
        ],
        "alternates": [transcript_out(turn) for turn in moment.alternates],
        "sources": list(moment.sources),
    }


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
                items.append(transcript_out(seg))
        return {"items": items}
    finally:
        store.close()


def review(limit: int = 50) -> ItemsOut:
    store = _store()
    try:
        return {"items": [transcript_out(s) for s in review_queue(store, limit=limit)]}
    finally:
        store.close()


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
            "before": [transcript_out(s) for s in nearby[max(0, idx - n) : idx]],
            "after": [transcript_out(s) for s in nearby[idx + 1 : idx + 1 + n]],
        }
    finally:
        store.close()
