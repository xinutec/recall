"""Maintenance passes: Opus transcoding, re-probing truncated segment rows."""

from __future__ import annotations

import os
import subprocess
import time
from datetime import UTC, datetime, timedelta
from pathlib import Path

from recall.ids import AudioSegmentId
from recall.maintenance import (
    backup_age_hours,
    compress_to_opus,
    reprobe_short_segments,
)
from recall.sources import AudioSource, SourceKind
from recall.store import Store
from recall.timeline import Segment

BASE = datetime(2026, 6, 13, 12, 0, 0, tzinfo=UTC)


def _sine(path: Path, seconds: float) -> None:
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
            str(seconds),
            "-ac",
            "1",
            "-c:a",
            "flac",
            str(path),
        ],
        check=True,
    )


def _add(
    store: Store, path: Path, *, seconds: float, at: float = 0.0
) -> AudioSegmentId:
    start = BASE + timedelta(seconds=at)
    return store.add_audio_segment(
        Segment(
            source_id="usb",
            sequence=0,
            start=start,
            end=start + timedelta(seconds=seconds),
            path=str(path),
            sample_rate=48000,
            channels=1,
        )
    )


def test_reprobe_extends_a_truncated_row_to_the_file_duration(tmp_path: Path) -> None:
    # Rows indexed while their file was still being written carry a short end_utc
    # forever (coverage, refine windows and trim clamps all believe it). Re-probing
    # measures the finalised file and repairs the end.
    audio_dir = tmp_path / "usb"
    audio_dir.mkdir()
    store = Store.memory()
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )

    # Indexed at 1s, but the finalised file is really 3s (the truncation case).
    truncated = audio_dir / "usb-20260613T120000.flac"
    _sine(truncated, 3.0)
    short_id = _add(store, truncated, seconds=1.0)
    # A genuinely short final segment (capture stopped): stored end == file length.
    honest = audio_dir / "usb-20260613T120100.flac"
    _sine(honest, 1.0)
    honest_id = _add(store, honest, seconds=1.0, at=60.0)

    repaired = reprobe_short_segments(store, now=9_999_999_999.0)

    assert repaired == 1
    fixed = store.audio_segment(short_id)
    assert fixed is not None
    assert (fixed.end - fixed.start).total_seconds() == 3.0
    kept = store.audio_segment(honest_id)
    assert kept is not None
    assert (kept.end - kept.start).total_seconds() == 1.0  # untouched

    # Idempotent: a second pass finds nothing left to repair.
    assert reprobe_short_segments(store, now=9_999_999_999.0) == 0


def test_reprobe_skips_fresh_and_missing_files(tmp_path: Path) -> None:
    audio_dir = tmp_path / "usb"
    audio_dir.mkdir()
    store = Store.memory()
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    # Still being written (mtime is now): not touched, however short the row says.
    fresh = audio_dir / "usb-20260613T120000.flac"
    _sine(fresh, 3.0)
    _add(store, fresh, seconds=1.0)
    # Row whose file is gone: skipped without crashing.
    _add(store, audio_dir / "usb-20260613T120100.flac", seconds=1.0, at=60.0)

    assert reprobe_short_segments(store, now=time.time()) == 0


def test_compress_replaces_flac_and_relinks(tmp_path: Path) -> None:
    audio_dir = tmp_path / "usb"
    audio_dir.mkdir()
    flac = audio_dir / "usb-20260613T120000.flac"
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
            "2",
            "-ac",
            "1",
            "-c:a",
            "flac",
            str(flac),
        ],
        check=True,
    )

    store = Store.memory()
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    audio_id = store.add_audio_segment(
        Segment(
            source_id="usb",
            sequence=0,
            start=BASE,
            end=BASE + timedelta(seconds=2),
            path=str(flac),
            sample_rate=48000,
            channels=1,
        )
    )

    count, reclaimed = compress_to_opus(store)

    assert count == 1
    opus = flac.with_suffix(".opus")
    assert opus.exists()
    assert not flac.exists()  # original removed
    assert reclaimed > 0  # opus is smaller

    # the store now points at the .opus file
    ref = store.audio_segment_ref(audio_id)
    assert ref is not None
    assert ref[0].endswith(".opus")

    # already-opus segments are skipped on a second pass
    again, _ = compress_to_opus(store)
    assert again == 0


def test_backup_age_reads_the_marker(tmp_path: Path) -> None:
    # The nightly mirror stamps .last-backup-ok on success; doctor alarms on a
    # stale or missing marker so a silently-dead backup can't go unnoticed.
    assert backup_age_hours(tmp_path, now=1_000_000.0) is None  # never backed up
    marker = tmp_path / ".last-backup-ok"
    marker.write_text("2026-07-02T23:30:00+00:00\n")
    os.utime(marker, (999_000.0, 999_000.0))
    age = backup_age_hours(tmp_path, now=1_000_000.0)
    assert age is not None
    assert abs(age - 1000.0 / 3600.0) < 1e-6
