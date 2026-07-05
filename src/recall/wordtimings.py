"""Give human-corrected turns audio timings by aligning their text to word-level ASR.

A machine turn carries per-word timings (recall.align); a human correction does not —
the typist supplied text, not timings. Splitting or tightly playing such a turn then has
no real boundary to snap to and falls back to character interpolation, which is badly
wrong on long turns (a 16-char phrase in a 1600-char turn maps to ~1% of the audio).

This re-derives word timings for the corrected *text* from word-level ASR of the same
audio: each text word takes the timing of the ASR word that produced it (a human
corrects what the model heard, so most words match — exactly or as a 1:1 substitution),
and words the human added are interpolated between their timed neighbours. Pure (text +
ASR words in, timings out), so it is fully unit-tested; the ASR call is the caller's.
"""

from __future__ import annotations

import difflib
from collections.abc import Sequence
from pathlib import Path
from typing import TYPE_CHECKING

from recall.asr import Transcriber, Word, scratch_wav, slice_clip

if TYPE_CHECKING:
    from recall.store import Store


_MIN_WORD_S = 0.02  # floor on a word's duration, so no split yields a zero-length turn


def _norm(word: str) -> str:
    """Letters/digits only, lowercased — so "Mask." matches the ASR's "mask"."""
    return "".join(ch for ch in word.lower() if ch.isalnum())


def align_text_to_audio(text: str, asr_words: Sequence[Word]) -> list[Word]:
    """Word timings for `text`, aligned to `asr_words` (the same audio's word ASR).

    Returns one `Word` per whitespace token of `text`, each with a leading space so the
    joined words reconstruct `text` (the format `recall.conversation._recut` reads).
    Empty if either input is empty.
    """
    tokens = text.split()
    if not tokens or not asr_words:
        return []

    a = [_norm(t) for t in tokens]
    b = [_norm(w.text) for w in asr_words]
    spans: list[tuple[float, float] | None] = [None] * len(tokens)
    for tag, i1, i2, j1, j2 in difflib.SequenceMatcher(
        a=a, b=b, autojunk=False
    ).get_opcodes():
        # "equal" matches outright; an equal-length "replace" is a 1:1 mis-hearing the
        # human fixed (alex -> alice) — map it positionally to keep the real timing.
        if tag == "equal" or (tag == "replace" and (i2 - i1) == (j2 - j1)):
            for k in range(i2 - i1):
                word = asr_words[j1 + k]
                spans[i1 + k] = (word.start, word.end)

    filled = _ensure_min_duration(_fill_gaps(spans, asr_words), asr_words[-1].end)
    return [
        Word(start=s, end=e, text=f" {tok}", probability=1.0)
        for tok, (s, e) in zip(tokens, filled, strict=True)
    ]


def _fill_gaps(
    spans: list[tuple[float, float] | None], asr_words: Sequence[Word]
) -> list[tuple[float, float]]:
    """Interpolate timings for unmatched tokens, spreading each run of them evenly
    across the gap between its surrounding matched neighbours."""
    n = len(spans)
    anchors = [i for i, s in enumerate(spans) if s is not None]
    if not anchors:
        # Nothing matched — spread the whole ASR span evenly over the tokens.
        t0, t1 = asr_words[0].start, asr_words[-1].end
        step = (t1 - t0) / n
        return [(t0 + i * step, t0 + (i + 1) * step) for i in range(n)]

    out: list[tuple[float, float]] = [s if s is not None else (0.0, 0.0) for s in spans]
    i = 0
    while i < n:
        if spans[i] is not None:
            i += 1
            continue
        j = i
        while j < n and spans[j] is None:  # a maximal run of unmatched tokens
            j += 1
        # Leading run spreads from the turn start (0.0) to the first match; trailing run
        # from the last match to the clip end — not collapsed onto the neighbour's time.
        lo = out[i - 1][1] if i > 0 else 0.0
        hi = out[j][0] if j < n else asr_words[-1].end
        hi = max(hi, lo)
        step = (hi - lo) / (j - i)
        for k in range(j - i):
            out[i + k] = (lo + k * step, lo + (k + 1) * step)
        i = j
    return out


def _ensure_min_duration(
    spans: list[tuple[float, float]], total: float
) -> list[tuple[float, float]]:
    """No word may have zero (or negative) duration — a degenerate word splits into a
    zero-length, audio-less turn. Nudge each to at least `_MIN_WORD_S`, keeping the run
    monotonic and clamped to the clip end (`total`). Wider words are left untouched."""
    out: list[tuple[float, float]] = []
    cursor = 0.0
    for start, end in spans:
        lo = max(start, cursor)
        hi = max(end, lo + _MIN_WORD_S)
        if total:
            hi = min(hi, total)
            lo = min(lo, max(0.0, hi - _MIN_WORD_S))
        out.append((lo, hi))
        cursor = hi
    return out


def backfill_word_timings(
    store: Store, transcriber: Transcriber, *, work_dir: Path, limit: int = 20
) -> int:
    """Fill current human-corrected turns that lack word timings: word-level ASR their
    audio and align the corrected text to it (`align_text_to_audio`). Off the request
    path; bounded per call. Returns how many were filled. A bad clip never crashes the
    pass. `transcriber` must produce word timings (mlx_transcribe with words=True)."""
    work_dir.mkdir(parents=True, exist_ok=True)
    filled = 0
    for turn in store.human_turns_missing_word_timings(limit=limit):
        if turn.audio_segment_id is None:
            continue
        ref = store.audio_segment_ref(turn.audio_segment_id)
        if ref is None:
            continue
        path, audio_start = ref
        rel_start = max(0.0, (turn.start - audio_start).total_seconds())
        rel_end = (turn.end - audio_start).total_seconds()
        with scratch_wav(work_dir / f"wt-{turn.id:06d}.wav") as clip:
            slice_clip(Path(path), clip, rel_start, rel_end)
            try:
                result = transcriber(clip)
            except Exception:  # a bad clip must never crash the always-on worker
                continue
        words = align_text_to_audio(turn.text, list(result.words))
        if words:
            store.set_word_timings(turn.id, words)
            filled += 1
    return filled
