"""Data-improvement passes derived from the data itself.

`scan_hallucinations` uses the VAD as a *labelling function* over the existing
archive: any current machine turn whose audio span contains no detected speech is
a confirmed silence-hallucination (the "Gracias."/"So" filler Whisper emits on an
empty room) and is soft-hidden with a reason — never deleted, fully recoverable.

The VAD is injected so this is testable with a stub; raw audio is untouched, so a
mistaken hide can be reverted and re-derived.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

from recall.quality import is_repetition_loop
from recall.store import Store
from recall.vad import Vad, overlaps_speech

HALLUCINATION_REASON = "no speech detected (VAD)"
LOOP_REASON = "repetition loop"


# A turn is hidden only when TWO signals agree: its audio is VAD-silent AND its
# text is repeated filler. Either alone is too noisy — VAD misses quiet far-field
# speech, and a repeated phrase can be genuine — so requiring both protects novel,
# real utterances (which appear in silence only by VAD error, never as filler).
DEFAULT_PAD_S = 1.0
DEFAULT_MIN_FILLER_COUNT = 8


@dataclass(frozen=True)
class ScanResult:
    segments_scanned: int
    turns_examined: int
    turns_hidden: int


def scan_loops(store: Store) -> int:
    """Soft-hide existing visible machine turns that are repetition loops.

    Pure text check (no audio decode, no Whisper) — instant and capture-safe.
    Returns how many were hidden.
    """
    hidden = 0
    for turn in store.visible_machine_turns():
        if is_repetition_loop(turn.text):
            store.hide(turn.id, LOOP_REASON)
            hidden += 1
    return hidden


def scan_hallucinations(
    store: Store,
    vad: Vad,
    *,
    pad_s: float = DEFAULT_PAD_S,
    min_filler_count: int = DEFAULT_MIN_FILLER_COUNT,
) -> ScanResult:
    """Hide repeated-filler machine turns that sit in VAD-detected silence.

    Two independent signals must agree, so neither alone destroys real data:
    - the text is filler (recurs >= `min_filler_count` times across the archive), and
    - the turn's span (padded by `pad_s` for timestamp slop) overlaps no speech.
    Human turns are never touched; hides are soft and recoverable.
    """
    filler = store.frequent_machine_texts(min_count=min_filler_count)
    scanned = examined = hidden = 0
    for audio_id in store.audio_segment_ids_with_machine_turns():
        ref = store.audio_segment_ref(audio_id)
        if ref is None:
            continue
        path, audio_start = ref
        if not Path(path).exists():
            continue
        regions = vad(Path(path))
        scanned += 1
        for turn in store.visible_machine_turns_for_audio(audio_id):
            examined += 1
            if turn.text not in filler:
                continue  # novel/one-off utterance — never hidden on VAD alone
            rel_start = (turn.start - audio_start).total_seconds() - pad_s
            rel_end = (turn.end - audio_start).total_seconds() + pad_s
            if not overlaps_speech(rel_start, rel_end, regions):
                store.hide(turn.id, HALLUCINATION_REASON)
                hidden += 1
    return ScanResult(
        segments_scanned=scanned, turns_examined=examined, turns_hidden=hidden
    )
