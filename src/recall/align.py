"""Assign a whole-segment transcription to diarized speakers, by word timing.

The high-quality path (vs. transcribing each diarized turn on its own): transcribe
a whole segment once — full context, so the language is detected reliably and the
anti-hallucination decoding works — then diarize it separately and assign each word
to whoever was speaking at that moment. This keeps the ASR context intact while
still splitting by speaker, at word granularity rather than a coarse midpoint.

Word timestamps (Whisper) and diarization boundaries (pyannote) are both ~100 ms
approximate, so a single word at a speaker boundary — or a one-word backchannel
("yeah") — routinely lands in the wrong span and would become its own spurious
turn. So after the raw per-word assignment we **smooth**: any run shorter than
`_MIN_TURN_S` is absorbed into a neighbour. Real speaking turns are longer than
that; sub-threshold "turns" are alignment artefacts.

**The overlap-aware rule is measured, and NOT shipped.** A turn change is a moment of
overlap: the incoming speaker starts before the outgoing one stops. pyannote's
*exclusive* diarization resolves that overlap in favour of the voice already talking,
so its boundary sits late and the incoming speaker's first few words land in the
previous turn. Passing `overlapping` (pyannote's other view, which keeps both speakers
over the contested stretch) decides each word by coverage instead, tie-broken by who is
still talking. It reads like the fix and it isn't: scored against two corrected
meetings it won one (+1.7pt near a speaker change) and lost the other (-4.7pt on 865
such words), 95.3% against 95.7% over both. It over-corrects — the non-exclusive view
extends *both* speakers across the handover, so coverage favours the incoming speaker's
longer span and a late boundary becomes an early one. `refine` therefore passes the
exclusive view alone. The parameter stays so `score-attribution` keeps scoring both,
and so the next attempt starts from a measurement rather than from this docstring.

Pure (words + speaker turns in, attributed runs out), so it's fully unit-tested.
"""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import dataclass

from recall.asr import Word
from recall.diarize import SpeakerTurn

# Runs shorter than this are treated as alignment artefacts (a jitter-flipped word, a
# backchannel) and folded into a neighbouring turn rather than kept as their own.
_MIN_TURN_S = 0.5


@dataclass(frozen=True)
class AlignedTurn:
    """A run of consecutive words attributed to one (relative) speaker."""

    speaker: str
    start: float
    end: float
    text: str
    confidence: float  # mean word probability across the run
    words: tuple[Word, ...]  # the run's words, for audio-exact boundary edits later


@dataclass
class _Run:
    """A mutable run of consecutive words by one speaker, during smoothing."""

    speaker: str
    words: list[Word]

    @property
    def duration(self) -> float:
        return self.words[-1].end - self.words[0].start


def _speaker_at(t: float, turns: list[SpeakerTurn]) -> str:
    """The relative speaker talking at time `t`: the turn containing it, or — if `t`
    falls in a gap between turns — the nearest one by edge distance."""
    for turn in turns:
        if turn.start <= t <= turn.end:
            return turn.speaker
    nearest = min(turns, key=lambda tr: min(abs(tr.start - t), abs(tr.end - t)))
    return nearest.speaker


def _speaker_over(
    word: Word, overlapping: Sequence[SpeakerTurn], exclusive: list[SpeakerTurn]
) -> str:
    """The speaker of `word`, decided on the overlap-aware view.

    Whoever covers more of the word wins; a tie goes to whoever is still talking
    latest. At a **handover** the incoming speaker covers the whole word while the
    outgoing one only catches its first moments, so the word goes to the incoming
    speaker. During a **backchannel** ("mm-hm" over someone mid-sentence) both cover the
    word entirely, and the tie-break gives it to the speaker who carries on.

    The measured flaw is in that first case: pyannote's non-exclusive view extends both
    speakers well past the actual handover, so "covers more" keeps choosing the incoming
    speaker for words the outgoing one really said, and the boundary lands early instead
    of late. Whatever replaces this needs a bound on how far a word may move, not a
    better tie-break. See the module docstring for the numbers.

    A word in a gap — no speaker active at all — falls back to the exclusive view's
    nearest turn; there is no overlap evidence to read there.
    """
    active = [t for t in overlapping if t.start < word.end and t.end > word.start]
    if not active:
        return _speaker_at((word.start + word.end) / 2.0, exclusive)
    covered: dict[str, float] = {}
    latest: dict[str, float] = {}
    for turn in active:
        overlap = min(word.end, turn.end) - max(word.start, turn.start)
        covered[turn.speaker] = covered.get(turn.speaker, 0.0) + max(0.0, overlap)
        latest[turn.speaker] = max(latest.get(turn.speaker, 0.0), turn.end)
    # The speaker name is the last key only so a total tie resolves deterministically.
    return max(covered, key=lambda s: (covered[s], latest[s], s))


def _coalesce(runs: list[_Run]) -> list[_Run]:
    """Merge adjacent runs that share a speaker into one."""
    merged: list[_Run] = []
    for run in runs:
        if merged and merged[-1].speaker == run.speaker:
            merged[-1].words.extend(run.words)
        else:
            merged.append(_Run(run.speaker, list(run.words)))
    return merged


def _smooth(runs: list[_Run], min_turn_s: float) -> list[_Run]:
    """Absorb sub-`min_turn_s` runs into a neighbour and re-coalesce, so a word or two
    flipped by timestamp jitter (or a backchannel) doesn't become its own turn. The
    shortest offender is relabelled to its longer neighbour each pass, until every run
    clears the threshold (or only one remains)."""
    while len(runs) > 1:
        tiny = [i for i, r in enumerate(runs) if r.duration < min_turn_s]
        if not tiny:
            break
        i = min(tiny, key=lambda j: runs[j].duration)
        left = runs[i - 1] if i > 0 else None
        right = runs[i + 1] if i + 1 < len(runs) else None
        if left is not None and right is not None:
            runs[i].speaker = (
                left.speaker if left.duration >= right.duration else right.speaker
            )
        elif left is not None:
            runs[i].speaker = left.speaker
        elif right is not None:
            runs[i].speaker = right.speaker
        runs = _coalesce(runs)
    return runs


def assign_words_to_speakers(
    words: list[Word],
    turns: list[SpeakerTurn],
    *,
    min_turn_s: float = _MIN_TURN_S,
    overlapping: Sequence[SpeakerTurn] | None = None,
) -> list[AlignedTurn]:
    """Group `words` into per-speaker runs, then smooth away sub-`min_turn_s` turns.
    The text is the words joined (Whisper words carry their own leading spaces).

    `turns` is the exclusive diarization. Pass `overlapping` (the overlap-aware view)
    to decide each word by coverage instead of by its midpoint. Both `min_turn_s` and
    `overlapping` are exposed so the attribution eval can score each setting against
    human ground truth; production passes neither — the module docstring records what
    the overlap-aware rule measured, and why it isn't the default.
    """
    if not words or not turns:
        return []

    raw: list[_Run] = []
    for word in words:
        speaker = (
            _speaker_at((word.start + word.end) / 2.0, turns)
            if overlapping is None
            else _speaker_over(word, overlapping, turns)
        )
        if raw and raw[-1].speaker == speaker:
            raw[-1].words.append(word)
        else:
            raw.append(_Run(speaker, [word]))

    aligned: list[AlignedTurn] = []
    for run in _smooth(raw, min_turn_s):
        text = "".join(w.text for w in run.words).strip()
        if text:
            aligned.append(
                AlignedTurn(
                    speaker=run.speaker,
                    start=run.words[0].start,
                    end=run.words[-1].end,
                    text=text,
                    confidence=sum(w.probability for w in run.words) / len(run.words),
                    words=tuple(run.words),
                )
            )
    return aligned
