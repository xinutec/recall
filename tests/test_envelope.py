"""The waveform the cleanup review draws: peak reduction, time alignment, real gaps."""

from __future__ import annotations

import math
from datetime import UTC, datetime, timedelta
from pathlib import Path

import numpy as np

from recall.envelope import (
    BUCKET_S,
    SILENCE_DB,
    EnvelopeSegment,
    build_envelope,
    find_events,
    peak_pool,
    rms_db,
    segment_envelope,
)
from recall.ids import AudioSegmentId

BASE = datetime(2026, 7, 11, 9, 0, 0, tzinfo=UTC)
QUIET = -62.0
LOUD = -20.0
THRESHOLD = -60.0


def _segment(i: int, *, seconds: float = 60.0, source: str = "usb") -> EnvelopeSegment:
    start = BASE + timedelta(seconds=i * seconds)
    return EnvelopeSegment(
        audio_id=AudioSegmentId(i),
        path=f"/archive/{source}/seg{i:03d}.opus",
        start=start,
        end=start + timedelta(seconds=seconds),
        mean_db=None,
    )


def _flat(db: float, seconds: float = 60.0) -> tuple[float, ...]:
    return (db,) * int(seconds / BUCKET_S)


def test_rms_db_of_silence_is_floored_not_infinite() -> None:
    silent = np.zeros((1, 800), dtype=np.float32)
    assert rms_db(silent)[0] == SILENCE_DB


def test_rms_db_reads_a_full_scale_tone_at_about_zero() -> None:
    # A full-scale sine is -3 dBFS RMS; the point is the scale is dBFS, not arbitrary.
    tone = np.sin(np.linspace(0, 20 * math.pi, 800, dtype=np.float32)).reshape(1, -1)
    assert -4.0 < rms_db(tone)[0] < -2.0


def test_peak_pool_keeps_the_loudest_not_the_average() -> None:
    # A short sound in a mostly-quiet bucket must survive zooming out — under-drawing
    # sound in a view used to approve deletion is the one unacceptable error.
    fine = [QUIET] * 9 + [LOUD]
    assert peak_pool(fine, 10) == [LOUD]


def test_peak_pool_reports_a_bucket_with_no_audio_as_a_gap() -> None:
    assert peak_pool([None] * 4, 4) == [None]
    # Partially covered: what audio exists still speaks.
    assert peak_pool([None, None, QUIET, None], 4) == [QUIET]


def test_envelope_places_segments_by_time_and_leaves_a_real_gap() -> None:
    # Segment 0 and segment 2 exist; the minute between them was never recorded. That
    # minute is unknown, not silent, so it must read as a gap — never as quiet.
    segments = [_segment(0), _segment(2)]
    envelope = build_envelope(
        segments,
        start=BASE,
        end=BASE + timedelta(seconds=180),
        threshold_db=THRESHOLD,
        max_points=1800,  # one point per 0.1s: no pooling, so alignment is exact
        envelope_of=lambda _path: _flat(QUIET),
    )
    assert envelope.bucket_s == BUCKET_S
    per_minute = [envelope.points[i * 600 : (i + 1) * 600] for i in range(3)]
    assert all(v == QUIET for v in per_minute[0])
    assert all(v is None for v in per_minute[1])
    assert all(v == QUIET for v in per_minute[2])


def test_envelope_shows_what_broke_the_quiet_at_the_edge() -> None:
    # The point of the whole view: the span is quiet, and the segment that ended it is
    # visibly loud — at the right place in time.
    quiet_then_loud = {
        _segment(0).path: _flat(QUIET),
        _segment(1).path: _flat(QUIET),
        _segment(2).path: _flat(QUIET)[:300] + _flat(LOUD)[:300],  # sound at 2m30s
    }
    envelope = build_envelope(
        [_segment(i) for i in range(3)],
        start=BASE,
        end=BASE + timedelta(seconds=180),
        threshold_db=THRESHOLD,
        max_points=1800,
        envelope_of=lambda path: quiet_then_loud[path],
    )
    loud_at = [
        i * envelope.bucket_s
        for i, v in enumerate(envelope.points)
        if v is not None and v > -40
    ]
    assert loud_at[0] == 150.0
    assert loud_at[-1] == 179.9


def test_envelope_coarsens_to_fit_max_points() -> None:
    envelope = build_envelope(
        [_segment(i) for i in range(30)],  # 30 minutes
        start=BASE,
        end=BASE + timedelta(minutes=30),
        threshold_db=THRESHOLD,
        max_points=900,
        envelope_of=lambda _path: _flat(QUIET),
    )
    assert envelope.bucket_s == 2.0  # 18000 fine buckets / 900 = pooled by 20
    assert len(envelope.points) == 900
    assert all(v == QUIET for v in envelope.points)


def _events(points: list[float | None]) -> list[tuple[float, float, float]]:
    found = find_events(points, start=BASE, bucket_s=BUCKET_S, threshold_db=THRESHOLD)
    return [
        (
            (e.start - BASE).total_seconds(),
            (e.end - BASE).total_seconds(),
            e.peak_db,
        )
        for e in found
    ]


def test_a_single_bump_in_a_quiet_stretch_is_one_event() -> None:
    # The reason the list exists: a 0.3s bump leaves a 60s mean well under the bar, so a
    # span offered for deletion still contains it — and someone has to hear it first.
    points: list[float | None] = [QUIET] * 100
    points[50:53] = [-50.0, -41.0, -48.0]
    assert _events(points) == [(5.0, 5.3, -41.0)]


def test_sounds_a_moment_apart_are_one_event_not_two() -> None:
    # Two syllables of the same word must not arrive as two things to check.
    points: list[float | None] = [QUIET] * 100
    points[50] = -45.0
    points[53] = -44.0  # 0.3s later, inside the 0.5s join gap
    assert _events(points) == [(5.0, 5.4, -44.0)]


def test_sounds_well_apart_are_separate_events() -> None:
    points: list[float | None] = [QUIET] * 100
    points[20] = -45.0
    points[60] = -44.0  # 4s later
    assert _events(points) == [(2.0, 2.1, -45.0), (6.0, 6.1, -44.0)]


def test_a_sound_running_to_the_end_of_the_window_still_reports() -> None:
    points: list[float | None] = [*([QUIET] * 10), *([-30.0] * 5)]
    assert _events(points) == [(1.0, 1.5, -30.0)]


def test_dead_air_yields_no_events() -> None:
    assert _events([QUIET] * 600) == []
    assert _events([None] * 600) == []


def test_events_are_found_on_the_fine_grid_not_the_zoomed_one() -> None:
    # Zoom must not change what you are asked to listen to: a 0.2s sound inside a
    # 30-minute view is still one event, even where a drawn bucket spans 2 seconds.
    loud_minute = _flat(QUIET)
    loud_minute = (*loud_minute[:300], -38.0, -39.0, *loud_minute[302:])
    envelope = build_envelope(
        [_segment(i) for i in range(30)],
        start=BASE,
        end=BASE + timedelta(minutes=30),
        threshold_db=THRESHOLD,
        max_points=900,  # heavily pooled: 2s per drawn bar
        envelope_of=lambda path: (
            loud_minute if path == _segment(7).path else _flat(QUIET)
        ),
    )
    assert envelope.bucket_s == 2.0
    assert len(envelope.events) == 1
    event = envelope.events[0]
    assert (event.start - BASE).total_seconds() == 7 * 60 + 30.0
    assert round(event.end.timestamp() - event.start.timestamp(), 1) == 0.2
    assert event.peak_db == -38.0


def test_a_corrupt_file_decodes_to_nothing_rather_than_raising(tmp_path: Path) -> None:
    # Real ffmpeg, real garbage: the archive holds a few unreadable segments, and one of
    # them must not take down the review of everything around it. Empty = a gap.
    broken = tmp_path / "broken.opus"
    broken.write_bytes(b"not an opus file")
    assert segment_envelope(str(broken)) == ()
    assert segment_envelope(str(tmp_path / "vanished.opus")) == ()


def test_undecodable_segment_reads_as_a_gap() -> None:
    # A corrupt or vanished file must never draw as silence — that would invite deleting
    # audio nobody could actually inspect.
    envelope = build_envelope(
        [_segment(0)],
        start=BASE,
        end=BASE + timedelta(seconds=60),
        threshold_db=THRESHOLD,
        max_points=600,
        envelope_of=lambda _path: (),
    )
    assert all(v is None for v in envelope.points)
