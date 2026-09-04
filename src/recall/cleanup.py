"""Data-improvement passes derived from the data itself.

`scan_hallucinations` uses the VAD as a *labelling function* over the existing
archive: any current machine turn whose audio span contains no detected speech is
a confirmed silence-hallucination (the "Gracias."/"So" filler Whisper emits on an
empty room) and is soft-hidden with a reason — never deleted, fully recoverable.

The VAD is injected so this is testable with a stub; raw audio is untouched, so a
mistaken hide can be reverted and re-derived.

Whisper's filler on an empty room is not only English. Measured over the archive,
the visible leftovers are 180 turns of non-Latin script (Cyrillic "лав", Japanese
"おやすみなさい") and 214 with no word in them at all ("...", "***"). `scan_loops`
would hide 8 more; that lever is spent. What is NOT here is a rule on the language
LABEL: 681 visible turns are labelled es/de/pt/tr, but only 180 are non-Latin
script — the rest are Dutch and English the model merely mislabelled, and hiding
those would erase real speech. Script is the safe signal; the language column is
not.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

from recall.ids import AudioSegmentId
from recall.quality import foreign_script_ratio, is_repetition_loop
from recall.store import Store, TranscriptSegment
from recall.vad import Vad, overlaps_speech

HALLUCINATION_REASON = "no speech detected (VAD)"
LOOP_REASON = "repetition loop"
FOREIGN_SCRIPT_REASON = "non-Latin script, no speech (VAD)"
EMPTY_TEXT_REASON = "no words"

# Above this fraction of non-Latin letters a turn is treated as foreign script.
# A half is deliberately blunt: real household text is ~0.0 (é and ü are Latin) and
# a hallucinated one is ~1.0, so nothing genuine sits near the line.
DEFAULT_MIN_FOREIGN_RATIO = 0.5

# Characters that carry no word. A turn made only of these says nothing about what
# was spoken, which is why hiding one cannot lose information. The unicode dashes
# and ellipsis are written as escapes: Whisper really does emit them, and spelled
# literally they are indistinguishable from ASCII to a reader.
_WORDLESS = (
    ". !?*-_,:;\"'()[]{}~/\\|@#$%^&+=<>`\t\n"
    "\u2026"  # horizontal ellipsis
    "\u00b7"  # middle dot
    "\u2013\u2014"  # en dash, em dash
)


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


def scan_foreign_script(
    store: Store,
    vad: Vad,
    *,
    pad_s: float = DEFAULT_PAD_S,
    min_ratio: float = DEFAULT_MIN_FOREIGN_RATIO,
) -> int:
    """Soft-hide machine turns in non-Latin script whose audio holds no speech.

    Two signals again, and for the same reason `scan_hallucinations` demands two: a
    visitor really can speak Japanese, and that is content, not noise. Script alone
    would erase them. Script AND silence cannot: nobody was speaking.

    Deliberately NOT a rule about short or low-confidence turns. The commonest
    low-confidence turns in this archive are "Yeah.", "Ja." and "Okay." — quiet
    real agreement, which in a verbal-memory aid is exactly the sort of thing worth
    keeping. Those are Latin script and untouched by this.

    Only candidate turns cost an audio decode, so this is a targeted pass rather
    than a sweep of every segment. Returns how many were hidden.
    """
    candidates: dict[AudioSegmentId, list[TranscriptSegment]] = {}
    for turn in store.visible_machine_turns():
        if turn.audio_segment_id is None:
            continue  # a live turn with no audio: no second signal to check
        if foreign_script_ratio(turn.text) > min_ratio:
            candidates.setdefault(turn.audio_segment_id, []).append(turn)
    hidden = 0
    for audio_id, turns in candidates.items():
        ref = store.audio_segment_ref(audio_id)
        if ref is None:
            continue
        path, audio_start = ref
        if not Path(path).exists():
            continue  # audio gone: no second signal, so never hide on script alone
        regions = vad(Path(path))
        for turn in turns:
            rel_start = (turn.start - audio_start).total_seconds() - pad_s
            rel_end = (turn.end - audio_start).total_seconds() + pad_s
            if not overlaps_speech(rel_start, rel_end, regions):
                store.hide(turn.id, FOREIGN_SCRIPT_REASON)
                hidden += 1
    return hidden


def scan_empty_text(store: Store) -> int:
    """Soft-hide machine turns containing no word at all — "...", "***", "!".

    The only single-signal rule here, and it needs no second one: the test is not
    "was this probably speech" but "does this text say anything", and the answer is
    no however loud the room was. Pure text, so like `scan_loops` it is instant and
    cannot compete with capture for the disk.
    """
    hidden = 0
    for turn in store.visible_machine_turns():
        if turn.text.strip(_WORDLESS).strip() == "":
            store.hide(turn.id, EMPTY_TEXT_REASON)
            hidden += 1
    return hidden
