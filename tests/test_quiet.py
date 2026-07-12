"""Grouping consecutive quiet capture segments into long spans to review for delete."""

from __future__ import annotations

import threading
import time
from datetime import UTC, datetime, timedelta

import pytest

from recall.envelope import Measurement
from recall.ids import AudioSegmentId
from recall.quiet import SWEEPABLE_KINDS, find_quiet_spans, quiet_spans, scan_segments
from recall.sources import AudioSource, SourceKind
from recall.store import SegmentVolume, Store
from recall.timeline import Segment

BASE = datetime(2026, 7, 11, 9, 0, 0, tzinfo=UTC)


def _quiet_measurement(_path: object) -> Measurement:
    """A minute of untouched noise floor, as the scan's single decode would see it."""
    return Measurement(mean_db=-62.0, buckets=(-62.0,) * 600)


def _add_source(store: Store, source_id: str, kind: SourceKind) -> None:
    store.add_source(AudioSource(id=source_id, name=source_id, kind=kind, spec=""))


def _add_segments(
    store: Store, source_id: str, n: int, *, first: int = 0
) -> list[AudioSegmentId]:
    """Captured *and* through ASR — the state a segment must reach before it can even be
    considered for deletion."""
    ids = []
    for i in range(first, first + n):
        start = BASE + timedelta(seconds=i * 59)
        audio_id = store.add_audio_segment(
            Segment(
                source_id=source_id,
                sequence=i,
                start=start,
                end=start + timedelta(seconds=59),
                path=f"/archive/{source_id}/seg{i:03d}.opus",
                sample_rate=48000,
                channels=1,
            )
        )
        store.mark_transcribed(int(audio_id))
        # The speech detector heard nothing: an unheard segment is unknown, never swept.
        store.set_audio_analysis(audio_id, speech_s=0.0, structure=0.4)
        ids.append(audio_id)
    return ids


def _store_with_capture(n: int) -> tuple[Store, list[AudioSegmentId]]:
    store = Store.memory()
    _add_source(store, "usb", SourceKind.COREAUDIO)
    return store, _add_segments(store, "usb", n)


def test_scan_caches_volumes_and_quiet_spans_finds_the_run(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    store, _ = _store_with_capture(8)
    # first 6 segments are noise-floor quiet, last 2 have sound
    vols = {
        f"/archive/usb/seg{i:03d}.opus": (-62.0 if i < 6 else -50.0) for i in range(8)
    }
    monkeypatch.setattr(
        "recall.quiet.measure",
        lambda p: Measurement(mean_db=vols[str(p)], buckets=(vols[str(p)],) * 600),
    )

    assert scan_segments(store) == 8
    assert store.audio_segments_unmeasured(kinds=SWEEPABLE_KINDS) == []  # all cached

    spans = quiet_spans(store, threshold_db=-60.0, min_duration_s=300.0)
    assert len(spans) == 1  # 6 * 59 = 354s of quiet, over the 300s minimum
    assert len(spans[0].audio_ids) == 6
    assert spans[0].source_id == "usb"


def test_the_scan_decodes_several_files_at_once(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # The scan is ~99.5% ffmpeg, and ffmpeg is a subprocess — so decodes must overlap.
    # Sequentially the archive takes ~50 minutes; eight at a time, ~20. If this ever
    # goes back to one-at-a-time the scan silently triples, so pin the concurrency.
    store, _ = _store_with_capture(16)
    live = 0
    peak = 0
    lock = threading.Lock()

    def slow_measure(_path: object) -> Measurement:
        nonlocal live, peak
        with lock:
            live += 1
            peak = max(peak, live)
        time.sleep(0.05)
        with lock:
            live -= 1
        return Measurement(mean_db=-62.0, buckets=(-62.0,) * 600)

    monkeypatch.setattr("recall.quiet.measure", slow_measure)
    assert scan_segments(store, workers=8) == 16
    assert peak > 1, "the scan decoded one file at a time"


def test_an_undecodable_file_is_recorded_as_examined_but_never_swept(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # A truncated file (capture died mid-write; the archive holds one) has no audio to
    # measure. Skipping it would leave it pending for ever — retried by every scan, and
    # the archive never reading as fully measured. So the verdict is recorded... but it
    # stays *unknown*: no volume, so it can never be swept into a delete, and it breaks
    # the quiet run it sits in rather than joining it.
    store, ids = _store_with_capture(13)
    broken = "/archive/usb/seg006.opus"
    monkeypatch.setattr(
        "recall.quiet.measure",
        lambda p: None if str(p) == broken else _quiet_measurement(p),
    )

    assert (
        scan_segments(store) == 13
    )  # examined, including the one that would not decode
    assert store.audio_segments_unmeasured(kinds=SWEEPABLE_KINDS) == []  # not retried

    volumes = {
        v.audio_id: v for v in store.audio_segment_volumes(kinds=SWEEPABLE_KINDS)
    }
    assert volumes[ids[6]].mean_db is None  # examined, but it has no volume

    spans = quiet_spans(store, threshold_db=-60.0, min_duration_s=300.0)
    swept = {a for span in spans for a in span.audio_ids}
    assert ids[6] not in swept  # never deleted...
    assert len(spans) == 2  # ...and it splits the quiet either side of it


def test_a_rescan_does_not_retry_a_file_that_will_never_decode(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    store, _ = _store_with_capture(4)
    calls: list[str] = []

    def counting(path: object) -> Measurement | None:
        calls.append(str(path))
        return None  # every file is broken

    monkeypatch.setattr("recall.quiet.measure", counting)
    assert scan_segments(store) == 4
    assert scan_segments(store) == 0  # nothing left to examine
    assert len(calls) == 4  # decoded once each, ever


def test_uploaded_meetings_are_never_scanned_or_swept(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # An imported meeting is the archive's most valuable audio. Even if a recording is
    # quiet enough to look like idle noise, it is never a delete candidate.
    store = Store.memory()
    _add_source(store, "meeting-1", SourceKind.UPLOAD)
    _add_segments(store, "meeting-1", 10)
    monkeypatch.setattr("recall.quiet.measure", _quiet_measurement)

    assert scan_segments(store) == 0  # not even measured — it can't be acted on
    assert quiet_spans(store, min_duration_s=300.0) == []


def test_two_mics_recording_at_once_do_not_share_a_span(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # The USB mic and a phone record the same room, so their segments interleave in
    # time. Runs group within a source: a span must never hold another mic's files.
    store = Store.memory()
    _add_source(store, "usb", SourceKind.COREAUDIO)
    _add_source(store, "pixel9", SourceKind.TCP_PCM)
    _add_segments(store, "usb", 10)
    _add_segments(store, "pixel9", 10)
    monkeypatch.setattr("recall.quiet.measure", _quiet_measurement)
    scan_segments(store)

    spans = quiet_spans(store, threshold_db=-60.0, min_duration_s=300.0)
    assert len(spans) == 2
    assert {s.source_id for s in spans} == {"usb", "pixel9"}
    for span in spans:
        paths = {store.audio_segment(a).path for a in span.audio_ids}  # type: ignore[union-attr]
        assert all(f"/{span.source_id}/" in p for p in paths)


def _seg(  # noqa: PLR0913 - one kwarg per veto, so each test states just its own
    i: int,
    mean_db: float | None,
    *,
    seconds: float = 59.0,
    source_id: str = "usb",
    at: float | None = None,
    transcribed: bool = True,
    has_speech: bool = False,
    speech_s: float | None = 0.0,
    structure: float | None = None,
) -> SegmentVolume:
    start = BASE + timedelta(seconds=i * seconds if at is None else at)
    return SegmentVolume(
        audio_id=AudioSegmentId(i),
        source_id=source_id,
        start=start,
        end=start + timedelta(seconds=seconds),
        mean_db=mean_db,
        transcribed=transcribed,
        has_speech=has_speech,
        speech_s=speech_s,
        structure=structure,
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


def test_a_recording_gap_breaks_a_run() -> None:
    # Capture stopped for an hour. That hour is *unknown*, not quiet: it must not carry
    # two short quiet runs over the minimum as if they were one long one.
    before = [_seg(i, -62.0, at=i * 59.0) for i in range(3)]  # 177s
    after = [_seg(10 + i, -62.0, at=3600.0 + i * 59.0) for i in range(3)]  # 177s
    assert find_quiet_spans(before + after, min_duration_s=300.0) == []


def test_quiet_either_side_of_a_gap_is_two_spans() -> None:
    long_run = [_seg(i, -62.0, at=i * 59.0) for i in range(6)]
    after_gap = [_seg(10 + i, -62.0, at=3600.0 + i * 59.0) for i in range(6)]
    spans = find_quiet_spans(long_run + after_gap, min_duration_s=300.0)
    assert len(spans) == 2
    assert spans[0].end < spans[1].start


def test_a_segment_bearing_speech_is_never_quiet() -> None:
    # The case from the real archive: a far-field Dutch sentence and a human-corrected
    # turn both sit on segments whose 60-second mean is under the noise-floor bar — a
    # few seconds of quiet speech barely move a minute's average. Deleting that audio
    # would take the transcript with it, so the turn vetoes the volume.
    segs = (
        [_seg(i, -62.0) for i in range(6)]
        + [_seg(6, -61.7, has_speech=True)]  # mean says quiet; a turn says otherwise
        + [_seg(i, -62.0) for i in range(7, 13)]
    )
    spans = find_quiet_spans(segs, threshold_db=-60.0, min_duration_s=300.0)
    assert len(spans) == 2  # it breaks the run...
    assert (
        AudioSegmentId(6) not in spans[0].audio_ids + spans[1].audio_ids
    )  # ...and is out


def test_an_untranscribed_segment_is_never_quiet() -> None:
    # ASR hasn't examined it yet: unknown, not empty. Never sweep what nothing has read.
    segs = [_seg(i, -62.0, transcribed=(i != 6)) for i in range(13)]
    spans = find_quiet_spans(segs, threshold_db=-60.0, min_duration_s=300.0)
    assert len(spans) == 2
    assert AudioSegmentId(6) not in spans[0].audio_ids + spans[1].audio_ids


def test_hidden_hallucinations_do_not_protect_a_segment() -> None:
    # The mirror image: a turn already hidden as a silence-hallucination ("Thank you."
    # on an empty room) is not speech, so it must not keep dead air alive forever.
    segs = [_seg(i, -62.0, has_speech=False) for i in range(6)]
    spans = find_quiet_spans(segs, threshold_db=-60.0, min_duration_s=300.0)
    assert len(spans) == 1
    assert len(spans[0].audio_ids) == 6


def test_store_marks_segments_that_still_bear_a_turn(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # End-to-end over the real store: the turn is what saves the audio, so the flag must
    # come from the same "current and visible" definition the rest of the app uses.
    store, ids = _store_with_capture(8)
    store.add_transcript_segment(
        audio_segment_id=int(ids[3]),
        start=BASE,
        end=BASE + timedelta(seconds=5),
        text="Namelijk, dit zijn ook al vlakjes",
        asr_model="whisper",
    )
    hidden = store.add_transcript_segment(
        audio_segment_id=int(ids[5]),
        start=BASE,
        end=BASE + timedelta(seconds=5),
        text="Thank you.",
        asr_model="whisper",
    )
    store.hide(hidden, reason="no speech detected (VAD)")
    monkeypatch.setattr("recall.quiet.measure", _quiet_measurement)
    scan_segments(store)

    volumes = {
        v.audio_id: v for v in store.audio_segment_volumes(kinds=SWEEPABLE_KINDS)
    }
    assert volumes[ids[3]].has_speech  # a turn that stands
    assert not volumes[ids[5]].has_speech  # hidden as a hallucination — not speech

    spans = quiet_spans(store, threshold_db=-60.0, min_duration_s=100.0)
    swept = {a for span in spans for a in span.audio_ids}
    assert ids[3] not in swept
    assert ids[5] in swept


def test_threshold_is_a_clean_cut() -> None:
    # -60 threshold: -62 quiet in, -55 (quietest real sound) out.
    segs = [_seg(i, -62.0) for i in range(5)] + [_seg(i, -55.0) for i in range(5, 10)]
    spans = find_quiet_spans(segs, threshold_db=-60.0, min_duration_s=200.0)
    assert len(spans) == 1
    assert spans[0].audio_ids == tuple(AudioSegmentId(i) for i in range(5))


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
