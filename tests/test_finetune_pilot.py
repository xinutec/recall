"""Pilot orchestration: export -> split -> eval base -> train -> eval adapter.

The heavy transcribe/train steps are injected as stubs, so the whole flywheel's
control flow is tested without any ML environment.
"""

from __future__ import annotations

import subprocess
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any

import pytest

from recall.finetune_pilot import run_pilot
from recall.review import apply_correction
from recall.sources import AudioSource, SourceKind
from recall.store import Store
from recall.timeline import Segment

BASE = datetime(2026, 6, 13, 12, 0, 0, tzinfo=UTC)
NOW = datetime(2026, 6, 13, 16, 0, 0, tzinfo=UTC)


def _seed_corpus(store: Store, tmp_path: Path, count: int) -> None:
    flac = tmp_path / "usb-20260613T120000.flac"
    subprocess.run(
        [
            "ffmpeg",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            str(count + 1),
            "-ac",
            "1",
            "-c:a",
            "flac",
            str(flac),
        ],
        check=True,
    )
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    audio_id = store.add_audio_segment(
        Segment(
            source_id="usb",
            sequence=0,
            start=BASE,
            end=BASE + timedelta(seconds=count + 1),
            path=str(flac),
            sample_rate=48000,
            channels=1,
        )
    )
    for i in range(count):
        seg = store.add_transcript_segment(
            audio_segment_id=audio_id,
            start=BASE + timedelta(seconds=i * 3),
            end=BASE + timedelta(seconds=i * 3 + 2.5),
            text=f"wrong text {i}",
            asr_model="whisper",
            language="nl",
            asr_confidence=0.5,
        )
        apply_correction(store, seg, f"this is correct text {i}", now=NOW)


def test_run_pilot_evals_base_then_adapter_on_the_same_holdout(tmp_path: Path) -> None:
    store = Store.memory()
    _seed_corpus(store, tmp_path, count=5)

    seen_adapters: list[Path | None] = []
    eval_sets: list[list[str]] = []

    def fake_transcribe(
        records: list[dict[str, Any]], adapter_dir: Path | None
    ) -> list[str]:
        seen_adapters.append(adapter_dir)
        eval_sets.append([r["text"] for r in records])
        if adapter_dir is None:
            return ["totally wrong"] * len(records)  # base: every word wrong
        return [r["text"] for r in records]  # adapter: perfect

    trained: list[Path] = []

    def fake_train(manifest: Path) -> Path:
        assert manifest.exists()  # the train split was written for training
        trained.append(manifest)
        return tmp_path / "adapter"

    report = run_pilot(
        store,
        tmp_path / "corpus",
        transcribe=fake_transcribe,
        train=fake_train,
        holdout=0.2,
    )

    # Split partitioned the corpus; both sides non-empty.
    assert report.test_count >= 1
    assert report.train_count >= 1
    assert report.train_count + report.test_count == 5
    # Base scored before training, adapter after — same held-out clips both times.
    assert seen_adapters == [None, tmp_path / "adapter"]
    assert eval_sets[0] == eval_sets[1]
    assert len(trained) == 1
    # The stub adapter is perfect and the stub base is all-wrong.
    assert report.base.wer > 0
    assert report.adapter.wer == 0.0
    assert report.delta > 0  # positive delta == adapter improved


def test_run_pilot_rejects_a_corpus_too_small_to_split(tmp_path: Path) -> None:
    store = Store.memory()
    _seed_corpus(store, tmp_path, count=1)

    def no_transcribe(
        records: list[dict[str, Any]], adapter_dir: Path | None
    ) -> list[str]:
        raise AssertionError("should not transcribe a too-small corpus")

    def no_train(manifest: Path) -> Path:
        raise AssertionError("should not train a too-small corpus")

    with pytest.raises(ValueError, match="too small"):
        run_pilot(
            store,
            tmp_path / "corpus",
            transcribe=no_transcribe,
            train=no_train,
            holdout=0.2,
        )
