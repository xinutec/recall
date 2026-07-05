"""The `finetune-pilot` CLI command: wiring + the capture-pause safety guard.

The heavy ML (transcribe/train) is stubbed; what's tested is that the command
runs the pilot end to end and pauses+resumes capture around the run.
"""

from __future__ import annotations

import subprocess
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any

import pytest

from recall import capture_control, cli
from recall.finetune import FinetuneConfig
from recall.review import apply_correction
from recall.sources import AudioSource, SourceKind
from recall.store import Store
from recall.timeline import Segment

BASE = datetime(2026, 6, 13, 12, 0, 0, tzinfo=UTC)
NOW = datetime(2026, 6, 13, 16, 0, 0, tzinfo=UTC)


def _seed_on_disk(out: Path, count: int) -> None:
    out.mkdir(parents=True, exist_ok=True)
    flac = out / "usb-20260613T120000.flac"
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
    store = Store.open(out / "recall.sqlite")
    try:
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
                text=f"wrong {i}",
                asr_model="whisper",
                language="nl",
                asr_confidence=0.5,
            )
            apply_correction(store, seg, f"this one is right {i}", now=NOW)
    finally:
        store.close()


def test_finetune_pilot_runs_and_brackets_the_run_with_capture_pause(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    out = tmp_path / "data"
    _seed_on_disk(out, count=5)

    def fake_transcribe(
        records: list[dict[str, Any]], *, base_model: str, adapter_dir: Path | None
    ) -> list[str]:
        if adapter_dir is None:
            return ["nonsense"] * len(records)  # base: wrong
        return [r["text"] for r in records]  # adapter: perfect

    def fake_finetune_lora(config: FinetuneConfig) -> Path:
        adapter = config.output_dir / "adapter"
        adapter.mkdir(parents=True, exist_ok=True)
        return adapter

    calls: list[str] = []

    def fake_pause(root: Path, now: datetime) -> datetime:
        calls.append("pause")
        return NOW

    def fake_resume(root: Path) -> None:
        calls.append("resume")

    monkeypatch.setattr(cli, "transcribe_clips", fake_transcribe)
    monkeypatch.setattr(cli, "finetune_lora", fake_finetune_lora)
    monkeypatch.setattr(capture_control, "pause", fake_pause)
    monkeypatch.setattr(capture_control, "resume", fake_resume)

    code = cli.main(["finetune-pilot", "--out", str(out)])

    assert code == 0
    # Capture was paused before and resumed after the heavy run.
    assert calls == ["pause", "resume"]
    output = capsys.readouterr().out
    assert "PILOT RESULT" in output
    assert "ADAPTER WINS" in output  # stub adapter is perfect, base is wrong


def test_finetune_pilot_no_pause_flag_leaves_capture_alone(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    out = tmp_path / "data"
    _seed_on_disk(out, count=5)

    monkeypatch.setattr(
        cli,
        "transcribe_clips",
        lambda records, *, base_model, adapter_dir: ["x"] * len(records),
    )
    monkeypatch.setattr(
        cli,
        "finetune_lora",
        lambda config: config.output_dir / "adapter",
    )

    def boom(*_a: object, **_k: object) -> None:
        raise AssertionError("capture must not be touched with --no-pause-capture")

    monkeypatch.setattr(capture_control, "pause", boom)
    monkeypatch.setattr(capture_control, "resume", boom)

    code = cli.main(["finetune-pilot", "--out", str(out), "--no-pause-capture"])
    assert code == 0
