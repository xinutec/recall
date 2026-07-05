"""The `correct` CLI command: substring-targeted human ASR corrections.

Dry-run leaves the store untouched; --apply supersedes the matched segment with
the corrected text while keeping its speaker. A fix that isn't a unique match is
skipped and makes the run exit non-zero; a malformed --fix is rejected by argparse.
"""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest

from recall import cli
from recall.sources import AudioSource, SourceKind
from recall.store import Store
from recall.timeline import Segment

BASE = datetime(2026, 6, 22, 9, 33, 0, tzinfo=UTC)
SESSION = "meeting-test"
OLD = "the mass pressure on the ice"
NEW = "the mask pressure on the eyes"
FIX = f"{OLD}=>{NEW}"


def _seed(out: Path, text: str) -> int:
    out.mkdir(parents=True, exist_ok=True)
    store = Store.open(out / "recall.sqlite")
    try:
        store.add_source(
            AudioSource(id=SESSION, name=SESSION, kind=SourceKind.COREAUDIO, spec="")
        )
        audio_id = store.add_audio_segment(
            Segment(
                source_id=SESSION,
                sequence=0,
                start=BASE,
                end=BASE + timedelta(seconds=5),
                path=str(out / "a.flac"),
                sample_rate=48000,
                channels=1,
            )
        )
        return store.add_transcript_segment(
            audio_segment_id=audio_id,
            start=BASE,
            end=BASE + timedelta(seconds=5),
            text=text,
            asr_model="test",
            speaker_label="Pippijn",
        )
    finally:
        store.close()


def _texts(out: Path) -> list[str]:
    store = Store.open(out / "recall.sqlite")
    try:
        return [t.text for t in store.session_turns(SESSION)]
    finally:
        store.close()


def test_dry_run_changes_nothing(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    _seed(tmp_path, OLD)
    code = cli.main(["correct", SESSION, "--fix", FIX, "--out", str(tmp_path)])
    assert code == 0
    assert _texts(tmp_path) == [OLD]  # untouched
    assert "DRY-RUN" in capsys.readouterr().out


def test_apply_writes_correction_and_keeps_speaker(tmp_path: Path) -> None:
    _seed(tmp_path, OLD)
    code = cli.main(
        ["correct", SESSION, "--fix", FIX, "--apply", "--out", str(tmp_path)]
    )
    assert code == 0
    store = Store.open(tmp_path / "recall.sqlite")
    try:
        turns = store.session_turns(SESSION)
        assert [t.text for t in turns] == [NEW]
        assert turns[0].speaker_label == "Pippijn"
    finally:
        store.close()


def test_non_unique_match_skips_and_fails(tmp_path: Path) -> None:
    _seed(tmp_path, "hello world")
    code = cli.main(
        ["correct", SESSION, "--fix", "not present=>x", "--out", str(tmp_path)]
    )
    assert code == 1
    assert _texts(tmp_path) == ["hello world"]


def test_malformed_fix_is_rejected(tmp_path: Path) -> None:
    _seed(tmp_path, "hello world")
    with pytest.raises(SystemExit):
        cli.main(["correct", SESSION, "--fix", "no delimiter", "--out", str(tmp_path)])
