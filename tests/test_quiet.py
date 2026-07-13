"""Grouping consecutive quiet capture segments into long spans to review for delete."""

from __future__ import annotations

import threading
import time
from datetime import UTC, datetime, timedelta

import pytest

from recall.envelope import Measurement, SpanSound
from recall.ids import AudioSegmentId
from recall.quiet import (
    SWEEPABLE_KINDS,
    QuietSpan,
    find_quiet_spans,
    quiet_spans,
    rank_spans,
    scan_segments,
)
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
    store, ids = _store_with_capture(8)
    # The first 6 are noise-floor quiet; the last 2 are louder *and* the detector heard
    # speech in them. Both facts are recorded, and it is the speech that keeps them out.
    vols = {
        f"/archive/usb/seg{i:03d}.opus": (-62.0 if i < 6 else -50.0) for i in range(8)
    }
    monkeypatch.setattr(
        "recall.quiet.measure",
        lambda p: Measurement(mean_db=vols[str(p)], buckets=(vols[str(p)],) * 600),
    )
    for i in (6, 7):
        store.set_audio_analysis(ids[i], speech_s=3.5, structure=0.9)

    assert scan_segments(store) == 8
    assert store.audio_segments_unmeasured(kinds=SWEEPABLE_KINDS) == []  # all cached

    spans = quiet_spans(store, min_duration_s=300.0)
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

    spans = quiet_spans(store, min_duration_s=300.0)
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

    spans = quiet_spans(store, min_duration_s=300.0)
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
    loud_fraction: float | None = 0.0,
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
        loud_fraction=loud_fraction,
    )


def test_long_run_of_quiet_is_one_span() -> None:
    # 10 * 59s = 590s of noise-floor quiet, over the 300s minimum.
    segs = [_seg(i, -62.0) for i in range(10)]
    spans = find_quiet_spans(segs, min_duration_s=300.0)
    assert len(spans) == 1
    assert spans[0].audio_ids == tuple(AudioSegmentId(i) for i in range(10))
    assert spans[0].duration_s == 590.0


def test_short_quiet_run_is_ignored() -> None:
    # 3 * 59s = 177s < 300s — normal between-utterance quiet, not a deletable span.
    segs = [_seg(i, -62.0) for i in range(3)]
    assert find_quiet_spans(segs, min_duration_s=300.0) == []


def test_speech_splits_quiet_into_separate_spans() -> None:
    # quiet(6) | speech | quiet(6) → two spans, the segment with speech in it excluded
    # from both. The detector is what splits them: it heard words there.
    segs = (
        [_seg(i, -62.0) for i in range(6)]
        + [_seg(6, -62.0, speech_s=2.4)]  # the VAD heard speech: it breaks the run
        + [_seg(i, -62.0) for i in range(7, 13)]
    )
    spans = find_quiet_spans(segs, min_duration_s=300.0)
    assert len(spans) == 2
    assert AudioSegmentId(6) not in spans[0].audio_ids + spans[1].audio_ids


def test_a_louder_minute_with_no_speech_stays_in_the_span() -> None:
    """The bug this rule was rewritten for. A door closes; the minute averages -56 dB
    against a -66 dB floor, and the detector hears no speech in it — because there is
    none.

    The old rule vetoed it on volume alone, and a single such minute cut a 90-minute
    silence into shards: 21 min | 6 min | 18 min | 42 min, all of them dead air. The
    ranking then rewarded the shards for being small and clean. Nine such minutes were
    checked against the detector on the real archive; not one held a word.
    """
    segs = (
        [_seg(i, -66.0) for i in range(6)]
        + [_seg(6, -56.6, loud_fraction=0.174)]  # a door: loud 17% of the minute
        + [_seg(i, -66.0) for i in range(7, 13)]
    )
    spans = find_quiet_spans(segs, min_duration_s=300.0)
    assert len(spans) == 1  # one span, not two
    assert AudioSegmentId(6) in spans[0].audio_ids  # and the bump is *in* it


def test_music_playing_to_an_empty_room_is_never_swept() -> None:
    """The case that the speech detector cannot see, and nearly lost.

    5 July, 17:27-17:44: ten minutes at -28 dB between two conversations about the
    songs playing ("I love this one", "I know this song. It was on a CD"). The detector
    hears no speech in music — correctly, there is none — so with volume removed as a
    veto it cleared all ten minutes for deletion, and the list offered them as "quiet".

    Music is not an empty room. The waveform says so and nothing else can: 88-100% of
    each of those minutes sits above the mic's own floor, where dead air is 0.2% and a
    door closing in an empty house is 31%.
    """
    segs = (
        [_seg(i, -66.0) for i in range(6)]
        + [_seg(6, -28.6, loud_fraction=1.0)]  # the room is full of music
        + [_seg(i, -66.0) for i in range(7, 13)]
    )
    spans = find_quiet_spans(segs, min_duration_s=300.0)
    assert len(spans) == 2  # it breaks the run...
    swept = spans[0].audio_ids + spans[1].audio_ids
    assert AudioSegmentId(6) not in swept  # ...and the music is out of both


def test_a_quiet_minute_the_detector_never_heard_is_never_swept() -> None:
    # speech_s is None: nobody has listened to it. Unknown is not empty — whatever its
    # mean says — and it breaks the run rather than joining it.
    segs = (
        [_seg(i, -62.0) for i in range(6)]
        + [_seg(6, -62.0, speech_s=None)]
        + [_seg(i, -62.0) for i in range(7, 13)]
    )
    spans = find_quiet_spans(segs, min_duration_s=300.0)
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
    spans = find_quiet_spans(segs, min_duration_s=300.0)
    assert len(spans) == 2  # it breaks the run...
    assert (
        AudioSegmentId(6) not in spans[0].audio_ids + spans[1].audio_ids
    )  # ...and is out


def test_an_untranscribed_segment_is_never_quiet() -> None:
    # ASR hasn't examined it yet: unknown, not empty. Never sweep what nothing has read.
    segs = [_seg(i, -62.0, transcribed=(i != 6)) for i in range(13)]
    spans = find_quiet_spans(segs, min_duration_s=300.0)
    assert len(spans) == 2
    assert AudioSegmentId(6) not in spans[0].audio_ids + spans[1].audio_ids


def test_hidden_hallucinations_do_not_protect_a_segment() -> None:
    # The mirror image: a turn already hidden as a silence-hallucination ("Thank you."
    # on an empty room) is not speech, so it must not keep dead air alive forever.
    segs = [_seg(i, -62.0, has_speech=False) for i in range(6)]
    spans = find_quiet_spans(segs, min_duration_s=300.0)
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

    spans = quiet_spans(store, min_duration_s=100.0)
    swept = {a for span in spans for a in span.audio_ids}
    assert ids[3] not in swept
    assert ids[5] in swept


def _span(minutes: float, *, at: float = 0.0) -> QuietSpan:
    start = BASE + timedelta(seconds=at)
    return QuietSpan(
        source_id="usb",
        start=start,
        end=start + timedelta(seconds=minutes * 60),
        audio_ids=(AudioSegmentId(int(at)),),
    )


def _sound(seconds: float, structure: float | None) -> SpanSound:
    """A span the detector heard no speech in, holding `seconds` of non-speech sound —
    a door, a cough. Nothing above the mic's floor at all when that is zero."""
    return SpanSound(
        loudest_db=-44.0 if seconds else None,
        margin_db=8.0 if seconds else -2.0,
        sound_seconds=seconds,
        structure=structure,
    )


def test_the_biggest_span_leads_the_list() -> None:
    """The list exists to reclaim disk, so it leads with what reclaims the most.

    It once ranked by *structure* — how featureless the audio is — which knows nothing
    of duration, so a spotless 6-minute shard led a list that also held a silent hour.
    Sorting the audio is not the same as sorting the work.
    """
    shard = (_span(6), _sound(0.0, 0.35))  # pristine, and worth six minutes
    hour = (_span(60, at=10_000), _sound(1.2, 0.71))  # two thumps, and worth an hour

    ranked = rank_spans([shard, hour])

    assert [s.duration_s for s, _ in ranked] == [3600.0, 360.0]


def test_a_bump_annotates_a_span_it_does_not_demote_it() -> None:
    # The sounds are carried, not hidden: the detector heard no speech in either of
    # these, but only a person can say whether an hour of empty room with two thumps in
    # it is worth keeping. Burying it below a shorter span would not be caution — it
    # would just be a slower way of not showing it.
    noisy = (_span(60), _sound(1.2, 0.9))
    silent_shorter = (_span(30, at=10_000), _sound(0.0, 0.3))

    ranked = rank_spans([silent_shorter, noisy])

    assert ranked[0][0].duration_s == 3600.0
    assert ranked[0][1].sound_seconds == 1.2  # and the review is told about the thumps
    assert not ranked[0][1].silent


def test_structure_breaks_a_tie_between_equals() -> None:
    # Same size, so the emptier audio goes first — the original ranking, demoted to
    # what it was always fit for: a tiebreak.
    busier = (_span(20), _sound(0.4, 0.8))
    emptier = (_span(20, at=10_000), _sound(0.0, 0.3))

    ranked = rank_spans([busier, emptier])

    assert ranked[0][1].structure == 0.3


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
