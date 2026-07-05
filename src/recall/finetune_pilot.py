"""End-to-end fine-tune pilot: does a LoRA on the corpus beat the base model?

Exports the corrections corpus, holds out a deterministic test split, transcribes
it with the base model, trains a LoRA on the rest, transcribes the same held-out
clips with the adapter, and reports the word-WER before vs after.

The heavy steps — transcribing with Whisper and training the LoRA — are injected
as callables (`transcribe`, `train`), so this orchestration is testable with
stubs and carries no ML imports itself. The CLI wires in the real
implementations from recall.finetune.
"""

from __future__ import annotations

import json
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from recall.evaluate import EvalReport, score_clips
from recall.store import Store
from recall.training import export_corpus, split_examples

# (held-out records, adapter dir or None for the base) -> one hypothesis per clip
Transcribe = Callable[[list[dict[str, Any]], Path | None], list[str]]
# (train manifest path) -> saved adapter directory
Train = Callable[[Path], Path]


@dataclass(frozen=True)
class PilotReport:
    """Before/after of one pilot run on the same held-out clips."""

    base: EvalReport
    adapter: EvalReport
    train_count: int
    test_count: int

    @property
    def delta(self) -> float:
        """WER points the adapter improved by (positive == adapter is better)."""
        return self.base.wer - self.adapter.wer

    @property
    def adapter_wins(self) -> bool:
        return self.delta > 0


def _load_manifest(path: Path) -> list[dict[str, Any]]:
    return [
        json.loads(line)
        for line in path.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]


def run_pilot(
    store: Store,
    dest: Path,
    *,
    transcribe: Transcribe,
    train: Train,
    holdout: float = 0.2,
) -> PilotReport:
    """Run the export -> eval-base -> train -> eval-adapter flywheel once.

    `dest` receives the exported corpus (clips/ + manifest.jsonl), the held-out
    train split (train.jsonl), and the trained adapter. Raises ValueError if the
    corpus is too small to split into non-empty train and test sets.
    """
    export_corpus(store, dest)
    records = _load_manifest(dest / "manifest.jsonl")
    train_records, test_records = split_examples(records, holdout=holdout)
    if not train_records or not test_records:
        raise ValueError(
            f"corpus too small to split for a pilot: {len(records)} examples"
        )

    train_manifest = dest / "train.jsonl"
    train_manifest.write_text(
        "".join(json.dumps(record) + "\n" for record in train_records),
        encoding="utf-8",
    )

    base = score_clips(test_records, transcribe(test_records, None))
    adapter_dir = train(train_manifest)
    adapter = score_clips(test_records, transcribe(test_records, adapter_dir))

    return PilotReport(
        base=base,
        adapter=adapter,
        train_count=len(train_records),
        test_count=len(test_records),
    )
