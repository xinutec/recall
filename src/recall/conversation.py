"""Managing who-says-what: assign a span of the transcript to a speaker.

A turn is just a run of words by one speaker, so changing who-said-what is one
operation — assign a text span (within a turn, or across several, with partial edges)
to a speaker. We split the turns at the span's edges, set the span's pieces to that
speaker, and let same-speaker neighbours read as one (the front-end coalesces them, so
*merge* needs no surgery). Splitting is the only surgery: it creates new turns and
**hides** the original — reversible, nothing deleted.

Split times are interpolated from the character position within the turn (we don't
store per-word timings); good enough for playback, exact for the text and ordering.
"""

from __future__ import annotations

from datetime import datetime, timedelta

from recall.asr import Word
from recall.store import ALIGNED_MARKER, DIARIZED_MARKER, Store

# Floor on a split piece's duration — so a collapsed cut never makes a zero-length,
# audio-less turn (e.g. a word that aligned to no audio).
_MIN_PIECE = timedelta(seconds=0.05)

# (start, end, text, speaker, word_timings) for one piece of a split.
_Piece = tuple[datetime, datetime, str, "str | None", "list[Word] | None"]


def _min_width_pieces(
    pieces: list[_Piece], turn_start: datetime, turn_end: datetime
) -> list[_Piece]:
    """Widen any zero/negative-width piece to a minimum playable span, keeping pieces
    ordered and clamped to the turn — so a collapsed cut never makes an audio-less turn.
    """
    out: list[_Piece] = []
    cursor = turn_start
    for start, end, chunk, speaker, words in pieces:
        lo = max(start, cursor)
        hi = min(max(end, lo + _MIN_PIECE), turn_end)
        if hi <= lo:  # out of room at the turn end — pull the start back
            lo = max(turn_start, hi - _MIN_PIECE)
        out.append((lo, hi, chunk, speaker, words))
        cursor = hi
    return out


def _nearest(values: list[int], target: int) -> int:
    """The element of `values` closest to `target` (ties → the earliest)."""
    return min(values, key=lambda v: abs(v - target))


def _snap_to_word(text: str, at: int) -> int:
    """Move `at` to the nearest word boundary, so a split never bisects a word."""
    at = max(0, min(at, len(text)))
    if 0 < at < len(text) and not text[at - 1].isspace() and not text[at].isspace():
        left = text.rfind(" ", 0, at)
        right = text.find(" ", at)
        if left < 0 and right < 0:
            return at
        if left < 0:
            return right
        if right < 0:
            return left
        return left if at - left <= right - at else right
    return at


def _recut(
    store: Store,
    turn_id: int,
    cuts: list[int],
    speakers: list[str | None],
    *,
    now: datetime,
) -> int:
    """Replace one turn with the pieces split at `cuts` (char offsets), each assigned
    the matching speaker from `speakers` (len == len(cuts)+1). Empty pieces drop out; a
    turn that ends up one piece is just relabelled in place. Returns 1 (one turn
    touched) or 0 (nothing to do). The original is hidden, not deleted."""
    turn = store.get_transcript(turn_id)
    if turn is None:
        return 0
    text = turn.text
    span = (turn.end - turn.start).total_seconds()
    words = turn.word_timings

    # (char offset of each word's start in `text`, time relative to the turn) plus a
    # final end boundary — for snapping cuts and playback to real word boundaries when
    # word timings exist. Empty otherwise: we then interpolate by character fraction
    # (the boundaries are estimates), preserving the old behaviour for old turns.
    boundaries: list[tuple[int, float]] = []
    word_chars: list[tuple[int, Word]] = []
    if words:
        joined = "".join(w.text for w in words)
        lead = len(joined) - len(joined.lstrip())  # leading ws stripped from turn.text
        pos = 0
        for w in words:
            char = min(len(text), max(0, pos - lead))
            boundaries.append((char, w.start))
            word_chars.append((char, w))
            pos += len(w.text)
        boundaries.append((len(text), words[-1].end))

    if boundaries:
        boundary_chars = [c for c, _ in boundaries]
        bounds = [0, *(_nearest(boundary_chars, c) for c in cuts), len(text)]
    else:
        bounds = [0, *(_snap_to_word(text, c) for c in cuts), len(text)]

    def at(char: int) -> datetime:
        # The turn's own edges are exact — a word's *timestamp* can sit inside leading
        # silence (or drift), so anchoring the first/last piece to it would drop the
        # turn's opening/closing audio. Only interior cuts snap to a word boundary.
        if char <= 0:
            return turn.start
        if char >= len(text):
            return turn.end
        if boundaries:
            _, rel = min(boundaries, key=lambda b: abs(b[0] - char))
            return turn.start + timedelta(seconds=rel)
        frac = char / len(text)
        return turn.start + timedelta(seconds=span * frac)

    def words_in(lo: int, hi: int, base: float) -> list[Word] | None:
        """The piece's own words, re-based to its start (None when no word timings)."""
        sel = [
            Word(w.start - base, w.end - base, w.text, w.probability)
            for char, w in word_chars
            if lo <= char < hi
        ]
        return sel or None

    pieces = []
    for k in range(len(bounds) - 1):
        lo, hi = bounds[k], bounds[k + 1]
        chunk = text[lo:hi].strip()
        if not chunk:
            continue
        start = at(lo)
        base = (start - turn.start).total_seconds()
        pieces.append((start, at(hi), chunk, speakers[k], words_in(lo, hi, base)))

    if not pieces:
        return 0
    if len(pieces) == 1:  # whole turn is one speaker — just relabel, no split
        store.set_turn_speaker(turn_id, pieces[0][3])
        return 1

    # Collapsed word times (a word with no audio) could make a zero-width, audio-less
    # turn; widen every piece to a minimum playable span first.
    pieces = _min_width_pieces(pieces, turn.start, turn.end)

    # Atomically claim the turn before creating any pieces: when the same turn is split
    # concurrently (an impatient double-tap firing several assigns at once), both read
    # it as live and reach here — only the caller that wins the claim splits it; the
    # others are no-ops instead of stamping out duplicate sets of pieces.
    if not store.claim_hidden(turn_id, f"split into pieces ({turn_id})"):
        return 0

    # Keep the parent's tier: a split of a diarized turn is still diarized-quality, so
    # tag the pieces with the aligned marker — the UI stays "finalized" (annotation
    # view, not the raw card view), and the aligned prefix keeps them out of the
    # re-diarize work-list. Splits of basic/human turns keep their own classification.
    parent_diarized = (turn.provenance or "").startswith(DIARIZED_MARKER)
    split_provenance = (
        f"{ALIGNED_MARKER} split of #{turn_id}"
        if parent_diarized
        else f"split of #{turn_id}"
    )
    for start, end, chunk, speaker, piece_words in pieces:
        store.add_transcript_segment(
            audio_segment_id=turn.audio_segment_id,
            start=start,
            end=end,
            text=chunk,
            asr_model=turn.asr_model,
            language=turn.language,
            language_confidence=turn.language_confidence,
            asr_confidence=turn.asr_confidence,
            speaker_label=speaker,
            speaker_cluster=turn.speaker_cluster,
            provenance=split_provenance,
            created=now,
            word_timings=piece_words,
        )
    return 1


def assign_span(  # noqa: PLR0913 - the span endpoints + the speaker
    store: Store,
    source_id: str,
    start_id: int,
    start_char: int,
    end_id: int,
    end_char: int,
    name: str,
    *,
    now: datetime,
) -> int:
    """Assign the text span from (start_id, start_char) to (end_id, end_char) to
    `name`. One gesture: the whole of a turn → reassign; part of one turn → split out
    that part; across turns → split the two edges and relabel everything between.
    Returns the number of turns touched."""
    ids = store.session_turn_ids(source_id)
    if start_id not in ids or end_id not in ids:
        return 0
    i, j = ids.index(start_id), ids.index(end_id)
    if i > j:  # selection made right-to-left
        i, j = j, i
        start_id, end_id = end_id, start_id
        start_char, end_char = end_char, start_char

    if start_id == end_id:
        turn = store.get_transcript(start_id)
        keep = turn.speaker_label if turn else None
        return _recut(
            store, start_id, [start_char, end_char], [keep, name, keep], now=now
        )

    start = store.get_transcript(start_id)
    end = store.get_transcript(end_id)
    touched = _recut(
        store,
        start_id,
        [start_char],
        [start.speaker_label if start else None, name],
        now=now,
    )
    for mid in ids[i + 1 : j]:
        store.set_turn_speaker(mid, name)
        touched += 1
    touched += _recut(
        store, end_id, [end_char], [name, end.speaker_label if end else None], now=now
    )
    return touched
