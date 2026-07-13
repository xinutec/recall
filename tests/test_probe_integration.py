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
from recall.probe import scan_segments, scan_source
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


def test_a_zero_byte_file_is_reported_as_a_tombstone_not_probed(tmp_path: Path) -> None:
    """46 of these sat in the real archive from June, unindexed and unmentioned.

    Capture opened the file and wrote nothing — it died on the spot. The scan used to
    hand each one to ffprobe, watch it fail, and `continue` with the note "retried next
    pass". They were: every worker pass, for three weeks, re-probed all 46 (an ffprobe
    *and* a full decode each), and because they were never indexed, nothing in the
    archive knew they existed.
    """
    source_dir = tmp_path / "usb"
    source_dir.mkdir()
    make_flac(source_dir / "usb-20260613T120000.flac", 1.0)
    dead = source_dir / "usb-20260613T120001.flac"
    dead.touch()  # zero bytes: capture opened it and died

    scan = scan_source(source_dir, "usb")

    kept = source_dir / "usb-20260613T120000.flac"
    assert [s.path for s in scan.segments] == [str(kept)]
    assert scan.empty == [dead]
    assert scan.unreadable == []


def test_a_file_still_being_written_is_never_called_dead(tmp_path: Path) -> None:
    # The guard that makes removing an empty file safe. Capture writes the header a
    # moment after opening the file, so a *live* segment is briefly zero bytes — and
    # deleting the file capture is recording into would destroy audio as it arrives.
    # The min-age bar keeps it out of the scan entirely, empty or not.
    source_dir = tmp_path / "usb"
    source_dir.mkdir()
    live = source_dir / "usb-20260613T120000.flac"
    live.touch()  # just opened by capture: zero bytes, this instant

    scan = scan_source(source_dir, "usb", min_age_seconds=120.0)

    assert scan.empty == []  # not dead — just young
    assert scan.segments == []


def test_a_corrupt_but_non_empty_file_is_reported_and_kept(tmp_path: Path) -> None:
    # Truncated mid-write: ffprobe refuses it, but there may be real audio in those
    # bytes. It is named in the log and left exactly where it is. Only a file with
    # nothing in it at all is removed.
    source_dir = tmp_path / "usb"
    source_dir.mkdir()
    corrupt = source_dir / "usb-20260613T120000.flac"
    corrupt.write_bytes(b"fLaC\x00\x00 truncated")

    scan = scan_source(source_dir, "usb")

    assert scan.unreadable == [corrupt]
    assert scan.empty == []
    assert corrupt.exists()
