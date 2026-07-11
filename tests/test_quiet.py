"""Grouping consecutive quiet capture segments into long spans to review for delete."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta

import pytest

from recall.ids import AudioSegmentId
from recall.quiet import SegmentVolume, find_quiet_spans, quiet_spans, scan_volumes
from recall.sources import AudioSource, SourceKind
from recall.store import Store
from recall.timeline import Segment

BASE = datetime(2026, 7, 11, 9, 0, 0, tzinfo=UTC)


def _store_with_capture(n: int) -> tuple[Store, list[AudioSegmentId]]:
    store = Store.memory()
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    ids = []
    for i in range(n):
        start = BASE + timedelta(seconds=i * 59)
        ids.append(
            store.add_audio_segment(
                Segment(
                    source_id="usb",
                    sequence=i,
                    start=start,
                    end=start + timedelta(seconds=59),
                    path=f"/archive/usb/seg{i:03d}.opus",
                    sample_rate=48000,
                    channels=1,
                )
            )
        )
    return store, ids


def test_scan_caches_volumes_and_quiet_spans_finds_the_run(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    store, _ = _store_with_capture(8)
    # first 6 segments are noise-floor quiet, last 2 have sound
    vols = {
        f"/archive/usb/seg{i:03d}.opus": (-62.0 if i < 6 else -50.0) for i in range(8)
    }
    monkeypatch.setattr("recall.quiet.measure_mean_volume", lambda p: vols[str(p)])

    assert scan_volumes(store) == 8
    assert store.audio_segments_without_volume() == []  # all cached now

    spans = quiet_spans(store, threshold_db=-60.0, min_duration_s=300.0)
    assert len(spans) == 1  # 6 * 59 = 354s of quiet, over the 300s minimum
    assert len(spans[0].audio_ids) == 6


def test_delete_audio_segments_removes_rows_and_returns_paths() -> None:
    store, ids = _store_with_capture(1)
    store.add_transcript_segment(
        audio_segment_id=int(ids[0]),
        start=BASE,
        end=BASE + timedelta(seconds=5),
        text="hi",
        asr_model="m",
    )
    paths = store.delete_audio_segments(ids)
    assert paths == ["/archive/usb/seg000.opus"]
    assert store.audio_segment(ids[0]) is None
    assert store.visible_machine_turns_for_audio(ids[0]) == []


def _seg(i: int, mean_db: float | None, *, seconds: float = 59.0) -> SegmentVolume:
    start = BASE + timedelta(seconds=i * seconds)
    return SegmentVolume(
        audio_id=AudioSegmentId(i),
        start=start,
        end=start + timedelta(seconds=seconds),
        mean_db=mean_db,
    )


def test_long_run_of_quiet_is_one_span() -> None:
    # 10 * 59s = 590s of noise-floor quiet, over the 300s minimum.
    segs = [_seg(i, -62.0) for i in range(10)]
    spans = find_quiet_spans(segs, threshold_db=-60.0, min_duration_s=300.0)
    assert len(spans) == 1
    assert spans[0].audio_ids == tuple(AudioSegmentId(i) for i in range(10))
    assert spans[0].duration_s == 590.0


def test_short_quiet_run_is_ignored() -> None:
    # 3 * 59s = 177s < 300s — normal between-utterance quiet, not a deletable span.
    segs = [_seg(i, -62.0) for i in range(3)]
    assert find_quiet_spans(segs, min_duration_s=300.0) == []


def test_speech_splits_quiet_into_separate_spans() -> None:
    # quiet(6) | speech | quiet(6) → two spans, the loud segment excluded from both.
    segs = (
        [_seg(i, -62.0) for i in range(6)]
        + [_seg(6, -50.0)]  # a real sound breaks the run
        + [_seg(i, -62.0) for i in range(7, 13)]
    )
    spans = find_quiet_spans(segs, threshold_db=-60.0, min_duration_s=300.0)
    assert len(spans) == 2
    assert AudioSegmentId(6) not in spans[0].audio_ids + spans[1].audio_ids


def test_unmeasured_segment_breaks_a_run() -> None:
    # None = couldn't measure; don't sweep an unknown segment into a delete.
    segs = (
        [_seg(i, -62.0) for i in range(6)]
        + [_seg(6, None)]
        + [_seg(i, -62.0) for i in range(7, 13)]
    )
    spans = find_quiet_spans(segs, min_duration_s=300.0)
    assert len(spans) == 2


def test_threshold_is_a_clean_cut() -> None:
    # -60 threshold: -62 quiet in, -55 (quietest real sound) out.
    segs = [_seg(i, -62.0) for i in range(5)] + [_seg(i, -55.0) for i in range(5, 10)]
    spans = find_quiet_spans(segs, threshold_db=-60.0, min_duration_s=200.0)
    assert len(spans) == 1
    assert spans[0].audio_ids == tuple(AudioSegmentId(i) for i in range(5))
