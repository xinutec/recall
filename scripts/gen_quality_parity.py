#!/usr/bin/env python3
"""Generate the parity corpus for `recalld::quality` from `recall.quality`.

The Rust port and the Python original must agree on every text, and shared unit
tests cannot show that: both sides were written to pass them. So the corpus is
generated — the Python is RUN over it and its verdicts are what the Rust is held
to — and it is generated rather than sampled from the archive because this
repository is PUBLIC and the archive is a household's conversations.

That trade is honest only if the generator reaches the branches real text
reaches, so the shapes below are the ones the archive actually contains: word
runs at the length threshold, phrase cycles at the repeat threshold, character
loops spanning just under and just over the length floor, punctuation-only
turns, and ordinary sentences as the controls. Nothing here is a household word.

Run:  nix develop --command .venv/bin/python scripts/gen_quality_parity.py
"""

from __future__ import annotations

import json
import random
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "src"))

from recall.cleanup import is_wordless
from recall.quality import is_repetition_loop

SEED = 20260912
COUNT = 400
FIXTURE = "recalld/tests/fixtures/quality-parity.json"
OUT = Path(__file__).resolve().parent.parent / FIXTURE

# Nonsense tokens at the lengths the RUN_WORD_MIN_LEN split is calibrated on:
# under six characters a back-to-back run is real emphasis, at six and over it is
# a hallucination. Both sides of the line have to be in the corpus or the port
# could move it and the test would not notice.
_SHORT = ["no", "ja", "wat", "hm", "oke", "toch"]
_LONG = ["momentum", "everything", "sembla", "coordinatie", "onderwerp", "gracias"]
# ⚠ **Exactly five and exactly six characters, and the corpus is worthless
# without them.** The first version of this file had no five-letter token at all,
# so moving RUN_WORD_MIN_LEN from 6 to 5 in the Rust changed no verdict and the
# parity test stayed green — it was pinning the rule everywhere except at the one
# place the rule is a decision. Every threshold below is straddled for the same
# reason: a corpus that never reaches a boundary cannot tell you the boundary
# moved.
_RUN_EDGE = ["vijfx", "zesxyz"]
_UNITS = ["ast", "oba", "go", "lala", "ndund", "xy"]
# Units of every length the character-loop rule accepts, and one past each end.
_UNIT_EDGE = ["q", "ab", "abcdefgh", "abcdefghi"]
_WORDLESS = ["...", "***", "!", " . . . ", "…", "--", "[ ]", "?!"]
# Distinct filler with no repeats of its own, for building texts whose verdict
# must come from the rule under test rather than from an accidental second loop.
_PLAIN_WORDS = [
    "kettle",
    "counter",
    "weekend",
    "afdelingen",
    "morgen",
    "arrive",
    "bedoelde",
    "tussen",
]
_PLAIN = [
    "the kettle is on the other counter",
    "ik denk dat we dat morgen moeten doen",
    "she said it would arrive before the weekend",
    "dat is niet wat ik bedoelde",
    "coordinatie tussen de twee afdelingen",
]


def _sentence(rng: random.Random) -> str:
    return rng.choice(_PLAIN)


def _word_run(rng: random.Random) -> str:
    """A single word repeated — either side of RUN_MIN and RUN_WORD_MIN_LEN."""
    return " ".join([rng.choice(_SHORT + _LONG)] * rng.randint(1, 9))


def _char_run(rng: random.Random) -> str:
    """A space-less unit repeated — either side of the character-loop floors."""
    return rng.choice(_UNITS) * rng.randint(2, 9)


def _mixed(rng: random.Random) -> str:
    """Ordinary words in no pattern: the control the other four are read against."""
    return " ".join(rng.choice(_SHORT + _LONG) for _ in range(rng.randint(1, 12)))


def _trailing_punctuation(rng: random.Random) -> str:
    """Words with a wordless tail — the case `str.strip(chars)` decides."""
    body = rng.choice(_PLAIN).split()
    rng.shuffle(body)
    return " ".join([*body, rng.choice(_WORDLESS)])


_SHAPES = (_sentence, _word_run, _char_run, _mixed, _trailing_punctuation)


def _cases(rng: random.Random) -> list[str]:
    out: list[str] = []
    # Deterministic corners first, so the corpus covers them whatever the rng does.
    out += _WORDLESS + _PLAIN
    out += [" ".join([w] * n) for w in _SHORT + _LONG + _RUN_EDGE for n in (2, 3, 4)]
    out += [u * n for u in _UNITS + _UNIT_EDGE for n in (3, 4, 5, 8)]
    out += [(u * n) + " tail" for u in _UNITS + _UNIT_EDGE for n in (4, 6)]
    # Straddle CHAR_LOOP_MIN_LEN (12 characters): four repeats of a two- and a
    # three-character unit are 8 and 12, and the rule turns over between them.
    out += ["ab" * 4, "abc" * 4, "ab" * 6, "abc" * 5]
    # Straddle WORD_MIN (6 words) with a text that is a loop by the dominance
    # rule alone, so the floor is what decides and not the back-to-back run.
    for total in (4, 5, 6, 7):
        half = ["momentum" if i % 2 == 0 else _PLAIN_WORDS[i] for i in range(total)]
        out.append(" ".join(half))
    # Straddle WORD_FRACTION (one token is half the words) exactly: 3-of-6 is the
    # line, 2-of-6 is under it, 4-of-6 over.
    for dominant in (2, 3, 4):
        out.append(" ".join(["momentum"] * dominant + _PLAIN_WORDS[: 6 - dominant]))
    # A phrase cycle either side of MIN_PHRASE_REPEATS, and either side of
    # MAX_PHRASE_WORDS.
    #
    # ⚠ **The phrase must be DISTINCT words, and drawing it randomly is how this
    # stopped being a test.** A random six-word phrase repeats a word often
    # enough that the back-to-back-run rule fires first, so moving
    # MAX_PHRASE_WORDS from 6 to 5 changed no verdict and the parity test stayed
    # green over the one period that is a decision. Distinct words, and a first
    # word unequal to the last so no run forms across the seam, leave the phrase
    # branch as the only rule that can answer.
    for period in (1, 2, 3, 5, 6, 7):
        phrase = _PLAIN_WORDS[:period]
        for reps in (2, 3, 4):
            out.append(" ".join(phrase * reps))
    while len(out) < COUNT:
        out.append(_SHAPES[rng.randrange(len(_SHAPES))](rng))
    return out[:COUNT]


def main() -> int:
    rng = random.Random(SEED)
    cases = [
        {"text": text, "loop": is_repetition_loop(text), "wordless": is_wordless(text)}
        for text in _cases(rng)
    ]
    OUT.write_text(json.dumps(cases, ensure_ascii=False) + "\n")
    loops = sum(1 for c in cases if c["loop"])
    wordless = sum(1 for c in cases if c["wordless"])
    print(f"{OUT.name}: {len(cases)} cases — {loops} loops, {wordless} wordless")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
