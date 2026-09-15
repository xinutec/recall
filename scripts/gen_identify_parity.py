#!/usr/bin/env python3
"""Generate the parity corpus for `recalld::identify` from `recall.identify`.

The Rust port decides WHOSE WORDS a turn is attributed to, and shared unit tests
cannot show the two agree: both sides were written to pass them. So the corpus is
generated and the PYTHON IS RUN over it — through a real `Store` and the real
`rematch_speaker_guesses`, not a re-implementation of its arithmetic here, which
would only pin what I believe that arithmetic is.

⚠ **Synthetic vectors, and that is a limitation rather than a preference.** This
repository is PUBLIC and the voiceprints are members of a household. What the
fixture cannot prove is that the two agree on real voices; a differential over the
live archive is the evidence for that and belongs in a task, not in a file here.

What it CAN reach is every branch that decides an attribution: one person with
several enrolled prints (the score is their BEST, not their mean), a near-tie
where the softmax is the whole answer, a runaway winner, a person enrolled once
against a person enrolled many times, and an embedding of zeros — which happens,
on silence, and must not become a NaN that loses every comparison.

Run:  nix develop --command .venv/bin/python scripts/gen_identify_parity.py
"""

from __future__ import annotations

import json
import random
import sys
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "src"))

from recall.identify import rematch_speaker_guesses
from recall.sources import AudioSource, SourceKind
from recall.store import Store
from recall.timeline import Segment

SEED = 20260915
DIM = 16
BASE = datetime(2026, 9, 6, 9, 45, tzinfo=UTC)


def unit(rng: random.Random) -> list[float]:
    return [rng.gauss(0.0, 1.0) for _ in range(DIM)]


def near(rng: random.Random, of: list[float], jitter: float) -> list[float]:
    return [x + rng.gauss(0.0, jitter) for x in of]


def main() -> int:
    rng = random.Random(SEED)
    store = Store.memory()
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    audio_id = store.add_audio_segment(
        Segment(
            source_id="usb",
            sequence=0,
            path="/x.flac",
            start=BASE,
            end=BASE.replace(minute=46),
            sample_rate=16000,
            channels=1,
        )
    )

    # Three people. Alice is enrolled FIVE times and from different "microphones"
    # (wide jitter); Bob once; Carol twice and close together. That asymmetry is
    # the archive's — 972 prints over 9 people — and it is what makes "best, not
    # mean" a decision rather than a detail.
    anchors = {"Alice": unit(rng), "Bob": unit(rng), "Carol": unit(rng)}
    enrolment = {"Alice": (5, 0.9), "Bob": (1, 0.0), "Carol": (2, 0.2)}
    for name, (count, jitter) in enrolment.items():
        for _ in range(count):
            store.enroll_speaker(name, near(rng, anchors[name], jitter), now=BASE)

    cases: list[dict[str, Any]] = []

    def turn(vector: list[float], label: str) -> None:
        tid = store.add_transcript_segment(
            audio_segment_id=audio_id,
            start=BASE,
            end=BASE.replace(minute=46),
            text=f"case {len(cases)}",
            asr_model="whisper",
            created=BASE,
        )
        store.set_embedding(tid, vector)
        cases.append({"id": tid, "shape": label, "embedding": vector})

    for name, anchor in anchors.items():
        turn(anchor, f"exactly-{name.lower()}")
        turn(near(rng, anchor, 0.3), f"near-{name.lower()}")
    # A near-tie: halfway between two anchors, where the softmax IS the answer.
    turn(
        [(a + b) / 2 for a, b in zip(anchors["Alice"], anchors["Bob"], strict=True)],
        "between-alice-and-bob",
    )
    # Nobody in particular.
    for _ in range(6):
        turn(unit(rng), "unrelated")
    # Silence. Must not divide by zero.
    turn([0.0] * DIM, "all-zeros")

    changed = rematch_speaker_guesses(store)
    # ⚠ Only rows the Python actually GUESSED go in. A case that came back
    # unguessed would pin "the port agrees that nothing happens", which is not the
    # property being checked — so it is a failure here rather than a weak case
    # there.
    guessed: dict[int, tuple[str, float]] = {}
    for segment_id, _vector, who, confidence in store.embeddings_with_guesses():
        if who is not None and confidence is not None:
            guessed[segment_id] = (who, confidence)
    for case in cases:
        person, score = guessed[case["id"]]
        case["expected_person"] = person
        case["expected_score"] = score

    voiceprints = [
        {"person": name, "vector": vec}
        for name, vectors in store.speaker_profiles().items()
        for vec in vectors
    ]
    out = (
        Path(__file__).resolve().parent.parent
        / "recalld/tests/fixtures/identify-parity.json"
    )
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(
        json.dumps(
            {
                "seed": SEED,
                "generated_by": "scripts/gen_identify_parity.py",
                "softmax_temperature": 0.1,
                "voiceprints": voiceprints,
                "cases": [
                    {
                        "shape": c["shape"],
                        "embedding": c["embedding"],
                        "expected_person": c["expected_person"],
                        "expected_score": c["expected_score"],
                    }
                    for c in cases
                ],
            },
            indent=1,
        )
        + "\n"
    )
    print(
        f"wrote {out} — {len(cases)} cases, "
        f"{len(voiceprints)} voiceprints, {changed} guessed"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
