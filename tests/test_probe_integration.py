"""Probe + timeline on real FLAC audio (deterministic, no mic, no live pipe).

Generates real FLAC files with controlled UTC timestamps in their names, then
exercises the actual ffprobe-based duration measurement, segment scanning, and
gap detection. The live capture pipeline (sox -> ffmpeg) is verified separately
against the real device.
"""

from __future__ import annotations

from datetime import timedelta
from pathlib import Path

from conftest import make_flac
from recall.probe import scan_segments
from recall.timeline import find_gaps


def test_contiguous_segments_have_no_gaps(tmp_path: Path) -> None:
    source_dir = tmp_path / "synth"
    source_dir.mkdir()
    for ts in ("20260613T120000", "20260613T120001", "20260613T120002"):
        make_flac(source_dir / f"synth-{ts}.flac", 1.0)

    segments = scan_segments(source_dir, "synth")
    assert len(segments) == 3
    assert all(s.sample_rate == 48000 for s in segments)
    # ~1.0s files spaced 1s apart -> continuous coverage
    assert find_gaps(segments, tolerance=timedelta(milliseconds=100)) == []


def test_scan_skips_known_paths(tmp_path: Path) -> None:
    source_dir = tmp_path / "synth"
    source_dir.mkdir()
    paths = [source_dir / f"synth-20260613T12000{i}.flac" for i in range(3)]
    for p in paths:
        make_flac(p, 1.0)

    # pretend the first two are already indexed -> only the third is returned
    known = frozenset(str(p) for p in paths[:2])
    segments = scan_segments(source_dir, "synth", known=known)
    assert [Path(s.path).name for s in segments] == ["synth-20260613T120002.flac"]


def test_missing_segment_is_detected_as_gap(tmp_path: Path) -> None:
    source_dir = tmp_path / "synth"
    source_dir.mkdir()
    # 120001 is missing -> a 1s hole between 120000 and 120002
    for ts in ("20260613T120000", "20260613T120002"):
        make_flac(source_dir / f"synth-{ts}.flac", 1.0)

    segments = scan_segments(source_dir, "synth")
    gaps = find_gaps(segments, tolerance=timedelta(milliseconds=100))
    assert len(gaps) == 1
    assert gaps[0].duration == timedelta(seconds=1)
