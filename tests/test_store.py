"""The searchable, versioned transcript store (SQLite + FTS5).

Encodes the core design decisions: outputs are derived views carrying model
version + confidence, superseded (never deleted) when a better pass replaces
them, and full-text searchable.
"""

from __future__ import annotations

import sqlite3
from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest

from recall.asr import Word
from recall.sources import AudioSource, SourceKind
from recall.store import _MIGRATIONS, RECONCILED_MARKER, SCHEMA_VERSION, Store
from recall.timeline import Segment

BASE = datetime(2026, 6, 13, 12, 0, 0, tzinfo=UTC)


def _source() -> AudioSource:
    return AudioSource(id="usb", name="USB", kind=SourceKind.COREAUDIO, spec="")


def _segment(start_s: float = 0.0, dur_s: float = 60.0) -> Segment:
    start = BASE + timedelta(seconds=start_s)
    return Segment(
        source_id="usb",
        sequence=0,
        start=start,
        end=start + timedelta(seconds=dur_s),
        path="x.flac",
        sample_rate=48000,
        channels=1,
    )


def test_diarize_skip_drops_a_segment_from_the_never_diarized_picker() -> None:
    # A segment the diarize coverage guard declined is journaled in diarize_skips, so
    # the newest-first `audio_segments_to_diarize` advances past it instead of the
    # daemon re-picking the same one forever (the live-lock: capture is paused, so no
    # newer segment ever bumps it out of the "newest" slot). A forced re-derive sees it.
    store = Store.memory()
    store.add_source(_source())
    a = store.add_audio_segment(_segment(0))
    b = store.add_audio_segment(_segment(120))
    for aid in (a, b):
        store.add_transcript_segment(
            audio_segment_id=aid,
            start=BASE,
            end=BASE + timedelta(seconds=3),
            text="hello there",
            asr_model="m",
        )
    assert set(store.audio_segments_to_diarize(limit=10)) == {a, b}

    store.mark_diarize_skipped(a, "coverage-guard (m)")
    assert store.audio_segments_to_diarize(limit=10) == [b]  # a advanced past
    assert store.is_diarize_skipped(a)
    # a forced source re-derive still sees it (skip table is scoped to the auto-pickers)
    assert a in store.audio_segments_for_source("usb", limit=10)

    store.clear_diarize_skip(a)
    assert set(store.audio_segments_to_diarize(limit=10)) == {a, b}  # back in the queue
    assert not store.is_diarize_skipped(a)


def test_diarize_skip_drops_a_segment_from_the_rediarize_picker() -> None:
    # The same skip also holds a segment out of the re-diarize (older-pipeline) queue,
    # so a guard-tripping segment can't live-lock that pass once the never-diarized
    # queue drains.
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment(0))
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=3),
        text="older pipeline turn",
        asr_model="m",
        provenance="diarized (old)",  # visible, old-pipeline → eligible for re-diarize
    )
    assert store.audio_segments_to_rediarize(limit=10) == [audio_id]

    store.mark_diarize_skipped(audio_id, "coverage-guard (m)")
    assert store.audio_segments_to_rediarize(limit=10) == []

    store.clear_diarize_skip(audio_id)
    assert store.audio_segments_to_rediarize(limit=10) == [audio_id]


def test_search_finds_inserted_text() -> None:
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=3),
        text="we need more coffee",
        asr_model="whisper-large-v3",
        language="en",
        language_confidence=0.99,
        asr_confidence=0.8,
    )
    results = store.search("coffee")
    assert len(results) == 1
    assert results[0].text == "we need more coffee"
    assert results[0].language == "en"
    assert results[0].asr_model == "whisper-large-v3"


def test_search_includes_the_capturing_source() -> None:
    # A search hit carries which recorder caught it (joined from its audio segment),
    # so "who said it, on which mic" is answerable from search alone.
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=3),
        text="we need more coffee",
        asr_model="whisper-large-v3",
    )
    results = store.search("coffee")
    assert len(results) == 1
    assert results[0].source_id == "usb"


def test_turns_by_id_returns_requested_turns_with_source_in_order() -> None:
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())
    a = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=1),
        text="first",
        asr_model="m",
    )
    b = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=2),
        end=BASE + timedelta(seconds=3),
        text="second",
        asr_model="m",
    )
    got = store.turns_by_id([b, a])
    assert [t.id for t in got] == [b, a]  # the requested order is preserved
    assert [t.text for t in got] == ["second", "first"]
    assert all(t.source_id == "usb" for t in got)  # capturing source joined in
    assert [t.id for t in store.turns_by_id([a, 999999])] == [a]  # missing id skipped


def test_turns_by_id_returns_a_superseded_turn() -> None:
    # You asked for that exact id, so it comes back even if no longer current.
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())
    old = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=1),
        text="old",
        asr_model="m",
    )
    new = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=1),
        text="new",
        asr_model="m",
    )
    store.supersede(old, new)
    got = store.turns_by_id([old])
    assert len(got) == 1
    assert got[0].superseded_by == new


def test_moment_coverage_separates_recorded_from_transcribed() -> None:
    # usb + two phones all record the window; usb gives 2 turns, pixel9 one, pixel5
    # has raw audio but no turns. A hidden turn doesn't inflate the count.
    store = Store.memory()
    for sid in ("usb", "pixel9", "pixel5"):
        store.add_source(
            AudioSource(id=sid, name=sid, kind=SourceKind.COREAUDIO, spec="")
        )

    def audio(sid: str) -> int:
        return store.add_audio_segment(
            Segment(
                source_id=sid,
                sequence=0,
                start=BASE,
                end=BASE + timedelta(seconds=60),
                path=f"{sid}.flac",
                sample_rate=48000,
                channels=1,
            )
        )

    u, p9, _p5 = audio("usb"), audio("pixel9"), audio("pixel5")

    def turn(audio_id: int, at: float) -> int:
        return store.add_transcript_segment(
            audio_segment_id=audio_id,
            start=BASE + timedelta(seconds=at),
            end=BASE + timedelta(seconds=at + 1),
            text="x",
            asr_model="m",
        )

    turn(u, 10)
    turn(u, 11)
    turn(p9, 10)
    store.hide(turn(u, 12), "test")  # hidden -> not counted

    cov = {
        c.source_id: c
        for c in store.moment_coverage(
            BASE + timedelta(seconds=9), BASE + timedelta(seconds=14)
        )
    }
    assert cov["usb"].recorded and cov["usb"].turns == 2
    assert cov["pixel9"].recorded and cov["pixel9"].turns == 1
    assert cov["pixel5"].recorded and cov["pixel5"].turns == 0  # recorded, no turns


def test_word_timings_round_trip() -> None:
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())
    tid = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=2),
        text="hello there",
        asr_model="m",
        word_timings=[Word(0.0, 0.5, "hello", 0.9), Word(0.6, 1.0, "there", 0.9)],
    )
    seg = store.get_transcript(tid)
    assert seg is not None
    assert seg.word_timings is not None
    assert [w.text for w in seg.word_timings] == ["hello", "there"]
    assert (seg.word_timings[0].start, seg.word_timings[1].end) == (0.0, 1.0)


def test_word_timings_default_to_none() -> None:
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())
    tid = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=1),
        text="x",
        asr_model="m",
    )
    seg = store.get_transcript(tid)
    assert seg is not None
    assert seg.word_timings is None


def test_provenance_and_created_round_trip() -> None:
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())
    tid = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=3),
        text="hi",
        asr_model="v1",
        provenance="whisper-large-v3 turbo",
        created=BASE,
    )
    seg = store.get_transcript(tid)
    assert seg is not None
    assert seg.provenance == "whisper-large-v3 turbo"
    assert seg.created == BASE


def test_supersede_many_records_lineage() -> None:
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())
    frags = [
        store.add_transcript_segment(
            audio_segment_id=audio_id,
            start=BASE + timedelta(seconds=i),
            end=BASE + timedelta(seconds=i + 1),
            text=f"frag{i}",
            asr_model="v1",
        )
        for i in range(3)
    ]
    merged = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=3),
        text="frag0 frag1 frag2",
        asr_model="v2-merge",
    )
    store.supersede_many(frags, merged)

    # the fragments drop out of the current view; the merge remains
    current = store.segments_in_range(BASE, BASE + timedelta(seconds=10))
    assert [s.id for s in current] == [merged]
    # lineage is auditable
    assert store.sources_of(merged) == sorted(frags)


def test_current_version_follows_supersede_chain() -> None:
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())
    v1 = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=3),
        text="one",
        asr_model="v1",
    )
    v2 = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=3),
        text="two",
        asr_model="v2",
    )
    v3 = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=3),
        text="three",
        asr_model="human",
    )
    store.supersede(v1, v2)
    store.supersede(v2, v3)
    # a link to the original resolves to the live version
    resolved = store.current_version(v1)
    assert resolved is not None
    assert resolved.id == v3
    assert resolved.text == "three"


def test_human_corrections_overlapping_by_audio_time() -> None:
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())
    tid = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=10),
        end=BASE + timedelta(seconds=14),
        text="x",
        asr_model="v1",
    )
    store.add_correction(
        transcript_segment_id=tid,
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=10),
        end=BASE + timedelta(seconds=14),
        original_text="x",
        corrected_text="ground truth",
        language="nl",
        created=BASE,
    )
    # a new turn whose span overlaps the corrected window finds it...
    hits = store.human_corrections_overlapping(
        audio_id, BASE + timedelta(seconds=12), BASE + timedelta(seconds=18)
    )
    assert [h.corrected_text for h in hits] == ["ground truth"]
    # ...a non-overlapping window does not
    assert (
        store.human_corrections_overlapping(
            audio_id, BASE + timedelta(seconds=20), BASE + timedelta(seconds=25)
        )
        == []
    )


def test_search_excludes_superseded_versions() -> None:
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())
    old = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=3),
        text="we need more coffee",
        asr_model="v1",
    )
    new = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=3),
        text="we need more coffee please",
        asr_model="v2",
    )
    store.supersede(old, new)

    results = store.search("coffee")
    assert len(results) == 1
    assert results[0].id == new
    assert results[0].text.endswith("please")


def test_segments_in_range_are_chronological() -> None:
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=2),
        end=BASE + timedelta(seconds=3),
        text="second",
        asr_model="v1",
    )
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=1),
        text="first",
        asr_model="v1",
    )
    rows = store.segments_in_range(
        BASE - timedelta(seconds=1), BASE + timedelta(seconds=10)
    )
    assert [r.text for r in rows] == ["first", "second"]


def test_segments_in_range_excludes_superseded() -> None:
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())
    old = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=1),
        text="old",
        asr_model="v1",
    )
    new = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=1),
        text="new",
        asr_model="v2",
    )
    store.supersede(old, new)
    rows = store.segments_in_range(
        BASE - timedelta(seconds=1), BASE + timedelta(seconds=10)
    )
    assert [r.text for r in rows] == ["new"]


def test_recent_transcripts_newest_first_and_paged() -> None:
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())
    for i in range(3):
        store.add_transcript_segment(
            audio_segment_id=audio_id,
            start=BASE + timedelta(seconds=i),
            end=BASE + timedelta(seconds=i + 1),
            text=f"turn-{i}",
            asr_model="v1",
        )

    newest = store.recent_transcripts(limit=2)
    assert [r.text for r in newest] == ["turn-2", "turn-1"]

    older = store.recent_transcripts(limit=10, before=newest[-1].start)
    assert [r.text for r in older] == ["turn-0"]


def test_recent_transcripts_pages_forward_with_after() -> None:
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())
    for i in range(4):
        store.add_transcript_segment(
            audio_segment_id=audio_id,
            start=BASE + timedelta(seconds=i),
            end=BASE + timedelta(seconds=i + 1),
            text=f"turn-{i}",
            asr_model="v1",
        )

    # forward paging: oldest-first, the page immediately newer than the cursor
    newer = store.recent_transcripts(limit=2, after=BASE)
    assert [r.text for r in newer] == ["turn-1", "turn-2"]
    nextp = store.recent_transcripts(limit=2, after=newer[-1].start)
    assert [r.text for r in nextp] == ["turn-3"]


def test_recent_transcripts_page_never_splits_a_same_timestamp_group() -> None:
    # Turns can share an exact start (co-located mics, a correction inheriting its
    # original's time). The paging cursor on the wire is start_utc ALONE, so a page
    # that cut such a group in half would make the next strict-< page skip the
    # group's remainder — turns silently missing from the timeline. A full page
    # therefore extends to swallow its boundary's ties.
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())

    def add(start: datetime, text: str) -> None:
        store.add_transcript_segment(
            audio_segment_id=audio_id,
            start=start,
            end=start + timedelta(seconds=1),
            text=text,
            asr_model="v1",
        )

    add(BASE, "oldest")
    for i in range(3):  # three turns at the SAME instant
        add(BASE + timedelta(seconds=10), f"tie-{i}")
    add(BASE + timedelta(seconds=20), "newest")

    # Newest-first, limit 2: the boundary lands inside the tie group → the page
    # grows to include all of it.
    page1 = store.recent_transcripts(limit=2)
    assert [r.text for r in page1] == ["newest", "tie-2", "tie-1", "tie-0"]
    page2 = store.recent_transcripts(limit=2, before=page1[-1].start)
    assert [r.text for r in page2] == ["oldest"]  # nothing skipped, nothing repeated

    # Forward paging, same rule.
    fwd1 = store.recent_transcripts(limit=2, after=BASE)
    assert [r.text for r in fwd1] == ["tie-0", "tie-1", "tie-2"]
    fwd2 = store.recent_transcripts(limit=2, after=fwd1[-1].start)
    assert [r.text for r in fwd2] == ["newest"]


def test_recent_transcripts_excludes_superseded() -> None:
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())
    old = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=1),
        text="old",
        asr_model="v1",
    )
    new = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=1),
        text="new",
        asr_model="v2",
    )
    store.supersede(old, new)
    assert [r.text for r in store.recent_transcripts()] == ["new"]


def test_training_queue_is_an_audible_band_clearest_first() -> None:
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())

    def turn(conf: float | None, text: str) -> int:
        # 3s / 4 words: clear of the backchannel filter — the band is the point.
        return store.add_transcript_segment(
            audio_segment_id=audio_id,
            start=BASE,
            end=BASE + timedelta(seconds=3),
            text=text,
            asr_model="v1",
            asr_confidence=conf,
        )

    turn(0.10, "unintelligible far field mumble")  # below floor -> excluded
    turn(0.50, "uncertain mid band turn")  # in band
    turn(0.80, "fairly clear spoken sentence")  # in band
    turn(0.97, "near certain crisp words")  # above ceiling -> excluded
    turn(None, "no confidence at all")  # NULL -> excluded

    rows = store.training_queue(min_confidence=0.35, max_confidence=0.9, limit=10)
    # only the band, clearest first
    assert [r.text for r in rows] == [
        "fairly clear spoken sentence",
        "uncertain mid band turn",
    ]


def test_training_queue_loudness_order_beats_confidence() -> None:
    # The candidate cap must not let confidence smuggle out loud turns: a loud but
    # low-confidence turn should rank ABOVE a quiet high-confidence one.
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())

    def turn(conf: float, text: str, at: float) -> int:
        # 3s / 4+ words: clear of the backchannel filter — ordering is the point.
        return store.add_transcript_segment(
            audio_segment_id=audio_id,
            start=BASE + timedelta(seconds=at),
            end=BASE + timedelta(seconds=at + 3),
            text=text,
            asr_model="v1",
            asr_confidence=conf,
        )

    quiet_confident = turn(0.90, "quiet but very confident", 10.0)
    loud_unsure = turn(0.50, "loud but quite unsure", 20.0)
    store.set_loudness(quiet_confident, 0.02)
    store.set_loudness(loud_unsure, 0.40)

    loud = store.training_queue(
        min_confidence=0.30, max_confidence=0.95, limit=10, order="loudness"
    )
    assert [r.text for r in loud] == [
        "loud but quite unsure",
        "quiet but very confident",
    ]

    chrono = store.training_queue(
        min_confidence=0.30, max_confidence=0.95, limit=10, order="time"
    )
    assert [r.text for r in chrono] == [
        "quiet but very confident",
        "loud but quite unsure",
    ]


def test_corrections_by_speaker_counts_each_voice() -> None:
    # The labelling UI needs per-voice counts to keep the corpus balanced across
    # the three speakers; untagged corrections fall under "".
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())

    def labelled(text: str, speaker: str | None) -> None:
        seg = store.add_transcript_segment(
            audio_segment_id=audio_id,
            start=BASE,
            end=BASE + timedelta(seconds=1),
            text="x",
            asr_model="v1",
            asr_confidence=0.5,
        )
        store.add_correction(
            transcript_segment_id=seg,
            audio_segment_id=audio_id,
            start=BASE,
            end=BASE + timedelta(seconds=1),
            original_text="x",
            corrected_text=text,
            language="nl",
            created=BASE,
            speaker=speaker,
        )

    labelled("a", "Alice")
    labelled("b", "Alice")
    labelled("c", "Carol")
    labelled("d", None)

    assert store.corrections_by_speaker() == {"Alice": 2, "Carol": 1, "": 1}


def test_review_list_reassign_and_hide_corrections() -> None:
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())

    def label(text: str, speaker: str) -> int:
        seg = store.add_transcript_segment(
            audio_segment_id=audio_id,
            start=BASE,
            end=BASE + timedelta(seconds=1),
            text="x",
            asr_model="v1",
            asr_confidence=0.5,
        )
        return store.add_correction(
            transcript_segment_id=seg,
            audio_segment_id=audio_id,
            start=BASE,
            end=BASE + timedelta(seconds=1),
            original_text="x",
            corrected_text=text,
            language="nl",
            created=BASE,
            speaker=speaker,
        )

    label("hoi", "Carol")
    cid_a = label("daag", "Alice")

    # list, filtered by voice
    assert {f.speaker for f in store.list_corrections()} == {"Carol", "Alice"}
    assert [f.text for f in store.list_corrections(speaker="Carol")] == ["hoi"]

    # re-assign moves the count
    carol = store.list_corrections(speaker="Carol")[0]
    store.set_correction_speaker(carol.correction_id, "Alice")
    assert store.corrections_by_speaker() == {"Alice": 2}

    # hide removes it from corpus, counts, and the review list
    store.hide_correction(cid_a, "wrong")
    assert store.correction_count() == 1
    assert cid_a not in {f.correction_id for f in store.list_corrections()}


def test_nudge_correction_span_extends_and_clamps() -> None:
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment(dur_s=60.0))  # BASE .. BASE+60s
    seg = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=10),
        end=BASE + timedelta(seconds=11),
        text="x",
        asr_model="v1",
        asr_confidence=0.5,
    )
    cid = store.add_correction(
        transcript_segment_id=seg,
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=10),
        end=BASE + timedelta(seconds=11),
        original_text="x",
        corrected_text="hi",
        language="nl",
        created=BASE,
        speaker="Carol",
    )

    store.nudge_correction(cid, "start", -0.5)  # start 0.5s earlier (widen)
    store.nudge_correction(cid, "end", 0.5)  # end 0.5s later (widen)
    frag = store.get_correction(cid)
    assert frag is not None
    assert (frag.start - BASE).total_seconds() == 9.5
    assert (frag.end - BASE).total_seconds() == 11.5

    store.nudge_correction(cid, "start", -100)  # can't go before the audio start
    frag = store.get_correction(cid)
    assert frag is not None
    assert frag.start == BASE  # clamped to the segment start


def test_nudge_turn_moves_one_edge_clamped() -> None:
    # Hand-tune a split boundary by ear: move start/end, clamped to the audio segment
    # and a 0.1s minimum span.
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment(dur_s=100.0))
    tid = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=10),
        end=BASE + timedelta(seconds=12),
        text="x",
        asr_model="diarized",
    )
    store.nudge_turn(tid, "start", -0.5)  # pull the start 0.5s earlier
    t = store.get_transcript(tid)
    assert t is not None
    assert t.start == BASE + timedelta(seconds=9.5)
    assert t.end == BASE + timedelta(seconds=12)
    store.nudge_turn(tid, "end", 0.5)  # push the end 0.5s later
    t = store.get_transcript(tid)
    assert t is not None and t.end == BASE + timedelta(seconds=12.5)
    store.nudge_turn(tid, "start", 100)  # would cross the end — clamped to min span
    t = store.get_transcript(tid)
    assert t is not None and t.end - t.start == timedelta(seconds=0.1)


def test_voiceprint_queue_offers_human_labelled_turns_gated() -> None:
    # Voiceprints derive from current human-labelled turns (speaker_label = a real
    # name), covering session-view assigns, not just text corrections. Each is offered
    # once, then never again (linked by source_segment_id). Gated for clip quality.
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment(dur_s=100.0))

    def turn(
        secs: float, name: str | None, *, dur: float = 2.0, loud: float = 0.1
    ) -> int:
        tid = store.add_transcript_segment(
            audio_segment_id=audio_id,
            start=BASE + timedelta(seconds=secs),
            end=BASE + timedelta(seconds=secs + dur),
            text="x",
            asr_model="diarized",
            speaker_label=name,
        )
        store.set_loudness(tid, loud)
        return tid

    good = turn(0, "Dr Lee")
    turn(10, None)  # unlabelled — never enrolment material
    turn(20, "SPEAKER_01")  # a raw cluster id, not a name — never
    turn(30, "Sam", dur=0.12)  # the one-word sliver — too short
    turn(40, "Sam", loud=0.0)  # near-silent — too faint

    pending = store.turns_needing_voiceprint()
    assert [(p.segment_id, p.speaker) for p in pending] == [(good, "Dr Lee")]

    store.enroll_speaker("Dr Lee", [0.1, 0.2], now=BASE, source_segment_id=good)
    assert store.turns_needing_voiceprint() == []


def test_prune_retires_replaced_and_stale_voiceprints() -> None:
    # Prints stay derived from current labels: a turn-sourced print whose turn was
    # re-assigned is dropped; a legacy print retires only once that speaker has a
    # turn-sourced one (gap-free), so a voice is never left print-less mid-rebuild.
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment(dur_s=100.0))

    def turn(secs: float, name: str) -> int:
        return store.add_transcript_segment(
            audio_segment_id=audio_id,
            start=BASE + timedelta(seconds=secs),
            end=BASE + timedelta(seconds=secs + 2),
            text="x",
            asr_model="diarized",
            speaker_label=name,
        )

    t_alice = turn(0, "Alice")
    t_bob = turn(10, "Bob")
    store.enroll_speaker("Alice", [0.1], now=BASE, source_correction_id=1)  # legacy
    store.enroll_speaker("Alice", [0.2], now=BASE, source_segment_id=t_alice)  # turn
    store.enroll_speaker("Bob", [0.3], now=BASE, source_segment_id=t_bob)
    store.enroll_speaker("Dana", [0.4], now=BASE, source_correction_id=2)  # legacy only
    store.set_turn_speaker(t_bob, "Carol")  # Bob's turn-sourced print is now stale

    assert store.prune_stale_voiceprints() == 2  # replaced legacy Alice + stale Bob
    profiles = store.speaker_profiles()
    # Bob gone; Dana's legacy kept (no turn-sourced replacement yet).
    assert set(profiles) == {"Alice", "Dana"}
    # Legacy Alice retired; one turn-sourced print left.
    assert len(profiles["Alice"]) == 1


def test_media_spans_finds_long_dense_runs() -> None:
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment(dur_s=2000.0))

    def turn(at: float, dur: float = 2.0) -> None:
        store.add_transcript_segment(
            audio_segment_id=audio_id,
            start=BASE + timedelta(seconds=at),
            end=BASE + timedelta(seconds=at + dur),
            text="x",
            asr_model="v1",
        )

    # a 10-minute dense run (back-to-back every 3s) = media
    for i in range(200):
        turn(i * 3.0)
    # a short isolated conversation later (gap > max_gap, short) = not media
    turn(2000.0)
    turn(2010.0)

    spans = store.media_spans(max_gap_s=20.0, min_duration_s=480.0)
    assert len(spans) == 1
    start, end = spans[0]
    assert (end - start).total_seconds() >= 480.0


def test_add_source_is_idempotent() -> None:
    store = Store.memory()
    store.add_source(_source())
    store.add_source(_source())  # must not raise


def test_add_audio_segment_dedups_by_source_and_start() -> None:
    store = Store.memory()
    store.add_source(_source())
    first = store.add_audio_segment(_segment(0))
    again = store.add_audio_segment(_segment(0))
    assert first == again


def _add_live(store: Store, at_s: float, text: str) -> int:
    return store.add_transcript_segment(
        audio_segment_id=None,
        start=BASE + timedelta(seconds=at_s),
        end=BASE + timedelta(seconds=at_s + 1),
        text=text,
        asr_model="live",
    )


def test_hide_provisional_covered_reconciles_only_spanned_live() -> None:
    # A live turn is reconciled only where a *transcribed* audio segment spans its
    # moment. Segment [0,60) is transcribed; [60,120) is not yet. The live turn at
    # t=90 (inside the un-transcribed segment) must survive — the archive has not
    # caught up to it — while t=5 (inside the transcribed one) is hidden.
    store = Store.memory()
    store.add_source(_source())
    covered = store.add_audio_segment(_segment(start_s=0, dur_s=60))
    store.add_audio_segment(_segment(start_s=60, dur_s=60))  # not transcribed yet
    store.mark_transcribed(covered)

    _add_live(store, 5, "live covered")
    _add_live(store, 90, "live pending")
    store.add_transcript_segment(
        audio_segment_id=covered,
        start=BASE + timedelta(seconds=2),
        end=BASE + timedelta(seconds=3),
        text="archive",
        asr_model="whisper",
    )

    assert store.hide_provisional_covered() == 1  # only the spanned live turn
    current = {
        s.text for s in store.segments_in_range(BASE, BASE + timedelta(seconds=120))
    }
    assert current == {"archive", "live pending"}
    # Idempotent.
    assert store.hide_provisional_covered() == 0


def test_hide_provisional_covered_spares_never_recorded_gap() -> None:
    # The bug: capture wrote empty files on start (cleared as dead stubs, so NO
    # audio segment) for the first stretch, then recorded normally. A blanket
    # "before the latest archive turn" watermark would hide the live turns in that
    # gap — their only record. Coverage-by-segment must keep them.
    store = Store.memory()
    store.add_source(_source())
    # Segment [0,60) was never recorded (empty stub cleared): no audio_segments row.
    later = store.add_audio_segment(_segment(start_s=120, dur_s=60))
    store.mark_transcribed(later)

    gap_live = _add_live(store, 30, "one two three")  # in the never-recorded gap
    _add_live(store, 130, "later live")  # spanned by the transcribed segment
    store.add_transcript_segment(
        audio_segment_id=later,
        start=BASE + timedelta(seconds=125),
        end=BASE + timedelta(seconds=126),
        text="archive later",
        asr_model="whisper",
    )

    assert store.hide_provisional_covered() == 1  # only the later, spanned turn
    resolved = store.current_version(gap_live)
    assert resolved is not None and resolved.text == "one two three"
    visible = {
        s.text for s in store.segments_in_range(BASE, BASE + timedelta(seconds=200))
    }
    assert visible == {"one two three", "archive later"}


def test_restore_uncovered_provisional_reverses_watermark_loss() -> None:
    # Recovery for turns the old watermark reconcile wrongly hid: un-hide reconciled
    # live turns no transcribed segment spans, leaving genuinely-covered ones hidden.
    store = Store.memory()
    store.add_source(_source())
    covered = store.add_audio_segment(_segment(start_s=120, dur_s=60))
    store.mark_transcribed(covered)

    lost = _add_live(store, 30, "lost count")  # never-recorded gap
    spanned = _add_live(store, 130, "reconciled ok")  # legitimately covered
    # Both were hidden by the old watermark pass.
    store.hide(lost, RECONCILED_MARKER)
    store.hide(spanned, RECONCILED_MARKER)

    assert store.restore_uncovered_provisional() == 1  # only the uncovered one
    visible = {
        s.text for s in store.segments_in_range(BASE, BASE + timedelta(seconds=200))
    }
    assert "lost count" in visible  # restored
    assert "reconciled ok" not in visible  # stays hidden (a segment spans it)
    # Idempotent.
    assert store.restore_uncovered_provisional() == 0


def test_speech_veto_searches_an_index_not_every_turn() -> None:
    """Quiet detection asks, per capture segment, "does a turn that still stands hang
    off this audio?" — the veto that stops a delete from destroying a transcript.

    It runs once per segment, so it must be a SEARCH. Unindexed it full-scanned all 44k
    turns for each of 9k segments (~410M row visits): /api/quiet/spans took 83 seconds.
    """
    store = Store.memory()
    plan = " ".join(
        str(value)
        for row in store._conn.execute(
            "EXPLAIN QUERY PLAN SELECT EXISTS (SELECT 1 FROM transcript_segments t "
            "WHERE t.audio_segment_id = 1 AND t.superseded_by IS NULL "
            "AND t.hidden_reason IS NULL)"
        ).fetchall()
        for value in row
    )
    assert "SEARCH t USING INDEX idx_ts_audio_current" in plan, plan


def test_enroll_and_speaker_profiles() -> None:
    store = Store.memory()
    ann = store.enroll_speaker("ann", [1.0, 0.0], now=BASE)
    store.enroll_speaker("ann", [0.9, 0.1], now=BASE)  # a second voiceprint
    store.enroll_speaker("bob", [0.0, 1.0], now=BASE)

    profiles = store.speaker_profiles()
    assert set(profiles) == {"ann", "bob"}
    assert len(profiles["ann"]) == 2
    assert profiles["ann"][0] == [1.0, 0.0]
    assert store.speaker_id_for("ann") == ann
    assert store.speaker_id_for("nobody") is None


def test_migrations_stamp_version_and_are_idempotent(tmp_path: Path) -> None:
    db = tmp_path / "recall.sqlite"
    store = Store.open(db)
    assert store.schema_version() == SCHEMA_VERSION
    store.add_source(_source())  # schema actually present
    store.close()

    # re-opening an up-to-date DB applies nothing and preserves data
    reopened = Store.open(db)
    assert reopened.schema_version() == SCHEMA_VERSION
    reopened.add_source(_source())  # idempotent, no duplicate-table error
    reopened.close()


def test_migrate_applies_only_pending_steps(tmp_path: Path) -> None:
    db = tmp_path / "recall.sqlite"
    # simulate an older database that's only at v1
    conn = sqlite3.connect(db)
    conn.executescript(_MIGRATIONS[0])
    conn.execute("PRAGMA user_version = 1")
    conn.commit()
    conn.close()

    # opening applies the remaining migrations and reaches the current version
    store = Store.open(db)
    assert store.schema_version() == SCHEMA_VERSION
    store.enroll_speaker("ann", [1.0, 0.0], now=BASE)  # exercises the v2 table
    assert "ann" in store.speaker_profiles()
    store.close()


def test_transaction_groups_writes_atomically() -> None:
    # A multi-step mutation (refine's hide-then-insert) must be all-or-nothing:
    # a crash between the steps must leave every step undone.
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())
    tid = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=1),
        text="old turn",
        asr_model="m",
    )

    def hide_then_crash() -> None:
        store.hide(tid, "test-marker")
        msg = "crash mid-write"
        raise RuntimeError(msg)

    with pytest.raises(RuntimeError, match="mid-write"), store.transaction():
        hide_then_crash()

    seg = store.get_transcript(tid)
    assert seg is not None
    assert seg.hidden_reason is None  # the hide rolled back with the crash

    with store.transaction():
        store.hide(tid, "test-marker")
        new = store.add_transcript_segment(
            audio_segment_id=audio_id,
            start=BASE,
            end=BASE + timedelta(seconds=1),
            text="new turn",
            asr_model="m",
        )
    seg = store.get_transcript(tid)
    assert seg is not None
    assert seg.hidden_reason == "test-marker"  # success path commits both
    assert store.get_transcript(new) is not None


def test_transactions_do_not_nest() -> None:
    store = Store.memory()
    with (
        pytest.raises(RuntimeError, match="nest"),
        store.transaction(),
        store.transaction(),
    ):
        pass  # pragma: no cover - unreachable


def test_migration_repairs_live_turns_superseded_by_unrelated_archive_turns(
    tmp_path: Path,
) -> None:
    """The repair step converts reconcile_live's old corrupt supersessions.

    Before the fix, reconcile_live pointed every caught-up live turn's
    superseded_by at ONE arbitrary archive turn — deep links then resolved to an
    unrelated utterance. The repair rewrites those to hidden (RECONCILED_MARKER)
    while leaving genuine human-correction supersessions alone.
    """
    db = tmp_path / "recall.sqlite"
    conn = sqlite3.connect(db)
    # Build the schema as it stood BEFORE the repair steps (v19+), then insert the
    # corrupt rows those steps exist to fix.
    for index, step in enumerate(_MIGRATIONS[:18]):
        conn.executescript(
            f"BEGIN;\n{step}\nPRAGMA user_version = {index + 1};\nCOMMIT;"
        )

    def add_turn(turn_id: int, model: str, superseded_by: int | None) -> None:
        conn.execute(
            """INSERT INTO transcript_segments
               (id, start_utc, end_utc, text, asr_model, superseded_by)
               VALUES (?, '2026-06-13T12:00:00+00:00', '2026-06-13T12:00:01+00:00',
                       'turn ' || ?, ?, ?)""",
            (turn_id, turn_id, model, superseded_by),
        )

    add_turn(1, "whisper", None)  # the arbitrary archive turn
    add_turn(2, "human", None)  # a human correction
    add_turn(3, "live", 1)  # corrupt: "superseded" by unrelated archive turn
    add_turn(4, "live", 2)  # genuine: superseded by its human correction
    # Corrupt AND already hidden: an old worker re-superseded a repaired row.
    add_turn(5, "live", 1)
    conn.execute(
        "UPDATE transcript_segments SET hidden_reason = 'live-reconciled' WHERE id = 5"
    )
    conn.commit()
    conn.close()

    store = Store.open(db)
    repaired = store.get_transcript(3)
    kept = store.get_transcript(4)
    rehidden = store.get_transcript(5)
    assert repaired is not None and kept is not None and rehidden is not None
    assert repaired.superseded_by is None
    assert repaired.hidden_reason == RECONCILED_MARKER
    assert kept.superseded_by == 2  # human supersession untouched
    assert kept.hidden_reason is None
    assert rehidden.superseded_by is None  # pointer cleared even on hidden rows
    assert rehidden.hidden_reason == RECONCILED_MARKER
    store.close()


def test_migration_step_is_atomic(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A migration that fails partway must roll back wholly.

    If a step's DDL commits but its version bump doesn't (a crash in the window
    between them, or a multi-statement step that fails after the first ALTER), the
    schema ends up ahead of user_version. The next startup then re-applies an
    already-applied ALTER and dies with 'duplicate column name' — a bricked DB.
    Each step must be atomic: all of it, or none.
    """
    db = tmp_path / "recall.sqlite"
    base = Store.open(db).schema_version()  # fully migrated to the current schema

    # A new step whose second statement fails *after* its ALTER has run — the
    # partial-apply trap. The whole step must roll back, not leave the column behind.
    bad = [
        *_MIGRATIONS,
        "ALTER TABLE sources ADD COLUMN atomicity_probe TEXT;\n"
        "INSERT INTO does_not_exist VALUES (1);",
    ]
    monkeypatch.setattr("recall.store._MIGRATIONS", bad)
    store = Store.connect(db)
    with pytest.raises(sqlite3.OperationalError):
        store.migrate()
    store.close()

    # The corrected step must now apply cleanly. With a non-atomic migrate the
    # column from the failed attempt lingers and this raises 'duplicate column'.
    good = [*_MIGRATIONS, "ALTER TABLE sources ADD COLUMN atomicity_probe TEXT;"]
    monkeypatch.setattr("recall.store._MIGRATIONS", good)
    store = Store.connect(db)
    store.migrate()
    assert store.schema_version() == base + 1
    store.close()


def test_persists_to_disk(tmp_path: Path) -> None:
    db = tmp_path / "recall.sqlite"
    store = Store.open(db)
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=1),
        text="persisted coffee",
        asr_model="v1",
    )
    store.close()

    reopened = Store.open(db)
    assert len(reopened.search("coffee")) == 1


def test_embedding_round_trips_and_drains_the_embed_worklist() -> None:
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())
    machine = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=2),
        text="een zin",
        asr_model="whisper",
        asr_confidence=0.5,
    )

    # An un-embedded machine turn is on the embed-once work-list.
    assert [s.id for s in store.segments_missing_embedding()] == [machine]

    store.set_embedding(machine, [0.1, 0.2, 0.3])
    # Once embedded it drops off the work-list (never re-embed)...
    assert store.segments_missing_embedding() == []
    # ...and the stored vector is available to the cheap re-match.
    assert store.embeddings_with_guesses() == [(machine, [0.1, 0.2, 0.3], None, None)]

    # The re-match writes guesses in bulk.
    store.set_speaker_guesses([(machine, "Alice", 0.31)])
    seg = store.get_transcript(machine)
    assert seg is not None
    assert seg.speaker_guess == "Alice"
    assert seg.speaker_score == 0.31
    assert store.embeddings_with_guesses() == [
        (machine, [0.1, 0.2, 0.3], "Alice", 0.31),
    ]


def test_embed_worklist_skips_human_confirmed_turns() -> None:
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())
    # A human-confirmed turn (speaker_label set) is authoritative — never embedded.
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=2),
        text="confirmed",
        asr_model="human",
        speaker_label="Carol",
    )
    assert store.segments_missing_embedding() == []


def test_embed_worklist_skips_too_short_clips() -> None:
    # A degenerate clip (near-zero or negative span) can't be embedded and crashes
    # pyannote — keep such turns off the work-list entirely.
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())
    good = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=1),
        text="long enough",
        asr_model="whisper",
    )
    store.add_transcript_segment(  # ~10ms — too short to embed
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(milliseconds=10),
        text="x",
        asr_model="whisper",
    )
    store.add_transcript_segment(  # negative span (end before start) — degenerate
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=2),
        end=BASE,
        text="y",
        asr_model="whisper",
    )
    assert [s.id for s in store.segments_missing_embedding()] == [good]


def test_session_summaries_names_only_confirmed_speakers() -> None:
    store = Store.memory()
    store.add_source(
        AudioSource(id="meeting-x", name="Meeting X", kind=SourceKind.UPLOAD, spec="")
    )
    audio_id = store.add_audio_segment(
        Segment(
            source_id="meeting-x",
            sequence=0,
            start=BASE,
            end=BASE + timedelta(seconds=10),
            path="m.mp3",
            sample_rate=48000,
            channels=1,
        )
    )
    # A human-confirmed speaker (a real name in speaker_label) — this IS shown.
    confirmed = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=1),
        text="hello",
        asr_model="m",
    )
    store._conn.execute(
        "UPDATE transcript_segments SET speaker_label = 'Pippijn' WHERE id = ?",
        (confirmed,),
    )
    # A confident voiceprint *guess* (Alice at 0.95) — a real false-match shape on a
    # doctor meeting. It must NOT be named: guesses aren't asserted in the summary.
    guessed = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=2),
        end=BASE + timedelta(seconds=3),
        text="world",
        asr_model="m",
    )
    store.set_speaker_guess(guessed, "Alice", 0.95)
    # A raw diarization cluster — never surfaced as a person.
    cluster = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=4),
        end=BASE + timedelta(seconds=5),
        text="again",
        asr_model="m",
    )
    store._conn.execute(
        "UPDATE transcript_segments SET speaker_label = 'SPEAKER_01' WHERE id = ?",
        (cluster,),
    )
    store._conn.commit()

    speakers = (store.session_summaries()[0][5] or "").split(",")
    assert "Pippijn" in speakers  # human-confirmed → named
    assert "Alice" not in speakers  # a guess, even at 0.95 → never asserted
    assert "SPEAKER_01" not in speakers  # raw cluster tag → never a person
    assert "unknown" in speakers  # the guessed + clustered turns read as unknown


def test_name_voice_labels_a_whole_cluster_in_a_source() -> None:
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())
    ids = [
        store.add_transcript_segment(
            audio_segment_id=audio_id,
            start=BASE + timedelta(seconds=i),
            end=BASE + timedelta(seconds=i + 1),
            text=f"turn {i}",
            asr_model="diarized",
            speaker_cluster=cluster,
        )
        for i, cluster in enumerate(["SPEAKER_01", "SPEAKER_00", "SPEAKER_01"])
    ]
    # naming a voice labels every turn of that cluster, and no other voice
    n = store.name_voice("usb", "SPEAKER_01", "Dr Lee")
    assert n == 2
    rng = store.segments_in_range(BASE, BASE + timedelta(seconds=10))
    rows = {r.id: r for r in rng}
    assert rows[ids[0]].speaker_label == "Dr Lee"
    assert rows[ids[2]].speaker_label == "Dr Lee"
    assert rows[ids[1]].speaker_label is None  # the other voice untouched
    # and it doesn't enrol a voiceprint (no correction recorded)
    assert store.correction_count() == 0
    # clearing a name removes it
    store.name_voice("usb", "SPEAKER_01", None)
    rng = store.segments_in_range(BASE, BASE + timedelta(seconds=10))
    rows = {r.id: r for r in rng}
    assert rows[ids[0]].speaker_label is None


def test_cluster_namings_returns_one_dominant_name_per_voice() -> None:
    # cluster_namings is the fleet→Mac label payload. It reports (source, cluster, name)
    # for every human-named voice, one row per voice — the whole set, so the Mac can
    # diff it. A cluster names one voice, so if a couple of turns were reassigned to
    # another name, the voice's dominant (most-turns) label wins.
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())
    for i, cluster in enumerate(["SPEAKER_00", "SPEAKER_00", "SPEAKER_01"]):
        store.add_transcript_segment(
            audio_segment_id=audio_id,
            start=BASE + timedelta(seconds=i),
            end=BASE + timedelta(seconds=i + 1),
            text=f"turn {i}",
            asr_model="diarized",
            speaker_cluster=cluster,
        )
    # No human labels yet → nothing to publish.
    assert store.cluster_namings() == []

    store.name_voice("usb", "SPEAKER_00", "Dr. Kosmin")
    store.name_voice("usb", "SPEAKER_01", "Pippijn")
    namings = {(n.source_id, n.cluster): n.name for n in store.cluster_namings()}
    assert namings == {
        ("usb", "SPEAKER_00"): "Dr. Kosmin",
        ("usb", "SPEAKER_01"): "Pippijn",
    }


def test_set_turn_speaker_reassigns_a_single_turn() -> None:
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())
    tid = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=1),
        text="x",
        asr_model="diarized",
        speaker_cluster="SPEAKER_00",
    )
    store.set_turn_speaker(tid, "you")
    seg = store.get_transcript(tid)
    assert seg is not None
    assert seg.speaker_label == "you"


def test_known_speaker_names_unions_enrolled_and_assigned_labels() -> None:
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=1),
        text="x",
        asr_model="diarized",
        speaker_label="Dr Lee",
        speaker_cluster="SPEAKER_00",
    )
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=1),
        end=BASE + timedelta(seconds=2),
        text="y",
        asr_model="diarized",
        speaker_cluster="SPEAKER_01",  # no label -> contributes no name
    )
    store.enroll_speaker("Alice", [1.0, 0.0, 0.0], now=BASE)
    names = store.known_speaker_names()
    assert "Dr Lee" in names  # an assigned label
    assert "Alice" in names  # an enrolled household voice
    assert "SPEAKER_01" not in names  # raw diarization clusters excluded


def test_session_voice_suggestions_separates_by_confidence_not_plurality() -> None:
    # BOTH clusters' plurality guess is the same household name (a visitor's voice
    # false-matching it), but only the genuine cluster is confident + unanimous. The
    # mixed, low-score cluster (the visitor) gets no suggestion — named by hand.
    store = Store.memory()
    store.add_source(
        AudioSource(id="meeting-x", name="M", kind=SourceKind.UPLOAD, spec="")
    )
    audio_id = store.add_audio_segment(
        Segment(
            source_id="meeting-x",
            sequence=0,
            start=BASE,
            end=BASE + timedelta(seconds=60),
            path="x",
            sample_rate=48000,
            channels=1,
        )
    )

    def turn(i: int, cluster: str) -> int:
        return store.add_transcript_segment(
            audio_segment_id=audio_id,
            start=BASE + timedelta(seconds=i),
            end=BASE + timedelta(seconds=i + 1),
            text=f"t{i}",
            asr_model="diarized",
            speaker_cluster=cluster,
        )

    for i in range(3):  # the real voice: unanimous, high score
        store.set_speaker_guess(turn(i, "SPEAKER_01"), "Pippijn", 0.85)
    for i, (nm, sc) in enumerate(  # the visitor: plurality Pippijn but mixed + weak
        [("Pippijn", 0.40), ("Alice", 0.35), ("Pippijn", 0.42)], start=10
    ):
        store.set_speaker_guess(turn(i, "SPEAKER_00"), nm, sc)

    assert store.session_voice_suggestions("meeting-x") == {"SPEAKER_01": "Pippijn"}


def _seg_at(start_s: float, dur_s: float, path: str) -> Segment:
    start = BASE + timedelta(seconds=start_s)
    return Segment(
        source_id="usb",
        sequence=int(start_s),
        start=start,
        end=start + timedelta(seconds=dur_s),
        path=path,
        sample_rate=48000,
        channels=1,
    )


def test_refine_request_queue_roundtrip() -> None:
    store = Store.memory()
    store.add_source(_source())
    rid = store.add_refine_request("usb", BASE, BASE + timedelta(minutes=5))
    pending = store.pending_refine_requests()
    assert [r.id for r in pending] == [rid]
    assert pending[0].source == "usb"
    assert (pending[0].start, pending[0].end) == (BASE, BASE + timedelta(minutes=5))
    store.mark_refine_request_done(rid)
    assert store.pending_refine_requests() == []


def test_ab_compare_run_queue_lifecycle() -> None:
    store = Store.memory()
    store.add_source(_source())
    run_id = store.add_ab_compare_run(
        "usb",
        None,
        None,
        model_a="turbo",
        model_b="adapter",
        base_model="large-v3",
    )

    # Queued: shows in pending and the list, status 'queued', no result yet.
    pending = store.pending_ab_compare_runs()
    assert [r.id for r in pending] == [run_id]
    job = pending[0]
    assert job.source == "usb"
    assert (job.start, job.end) == (None, None)
    assert (job.model_a, job.model_b) == ("turbo", "adapter")
    assert job.base_model == "large-v3"
    assert job.status == "queued"
    assert job.mean_wer_a is None and job.result_json is None

    # Running: removed from the pending queue.
    store.mark_ab_compare_running(run_id)
    assert store.pending_ab_compare_runs() == []
    assert store.get_ab_compare_run(run_id).status == "running"  # type: ignore[union-attr]

    # Done: result_json + denormalized summary stored; list omits the heavy json.
    store.save_ab_compare_result(
        run_id,
        result_json='{"hello": "world"}',
        mean_wer_a=0.21,
        mean_wer_b=0.25,
        n_corrections=16,
        n_segments=1,
        n_changed=1,
    )
    full = store.get_ab_compare_run(run_id)
    assert full is not None
    assert full.status == "done"
    assert full.result_json == '{"hello": "world"}'
    assert (full.mean_wer_a, full.mean_wer_b) == (0.21, 0.25)
    assert (full.n_corrections, full.n_segments, full.n_changed) == (16, 1, 1)
    listed = store.list_ab_compare_runs()
    assert [r.id for r in listed] == [run_id]
    assert listed[0].result_json is None  # list view omits the large payload
    assert listed[0].mean_wer_b == 0.25


def test_ab_compare_run_window_and_error() -> None:
    store = Store.memory()
    store.add_source(_source())
    window_end = BASE + timedelta(minutes=5)
    run_id = store.add_ab_compare_run(
        "usb",
        BASE,
        window_end,
        model_a="a",
        model_b="b",
        base_model="base",
    )
    job = store.get_ab_compare_run(run_id)
    assert job is not None
    assert (job.start, job.end) == (BASE, window_end)

    store.mark_ab_compare_error(run_id, "boom")
    failed = store.get_ab_compare_run(run_id)
    assert failed is not None
    assert failed.status == "error"
    assert failed.error == "boom"
    assert store.pending_ab_compare_runs() == []
    assert store.get_ab_compare_run(9999) is None


def test_audio_segments_in_range_returns_only_overlapping() -> None:
    store = Store.memory()
    store.add_source(_source())
    a = store.add_audio_segment(_seg_at(0, 60, "a.flac"))  # [0, 60)
    b = store.add_audio_segment(_seg_at(60, 60, "b.flac"))  # [60, 120)
    store.add_audio_segment(_seg_at(120, 60, "c.flac"))  # [120, 180) — outside
    got = store.audio_segments_in_range(
        "usb", BASE + timedelta(seconds=50), BASE + timedelta(seconds=70), limit=100
    )
    assert set(got) == {a, b}


def test_nudge_turn_rebases_word_timings_with_the_start_edge() -> None:
    # Word timings are stored relative to the turn START. Trimming the start edge
    # moves that base — without re-basing, every later audio-exact split and tight
    # playback is off by exactly the trim.
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())
    tid = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=10),
        end=BASE + timedelta(seconds=13),
        text="one two three",
        asr_model="m",
        word_timings=[
            Word(0.0, 1.0, " one", 1.0),
            Word(1.0, 2.0, " two", 1.0),
            Word(2.0, 3.0, " three", 1.0),
        ],
    )

    store.nudge_turn(tid, "start", 1.5)  # start moves +1.5s (into "two")

    seg = store.get_transcript(tid)
    assert seg is not None
    assert seg.word_timings is not None
    # " one" is entirely before the new start -> dropped; " two" is clipped to the
    # new base; " three" shifts by -1.5.
    assert [w.text for w in seg.word_timings] == [" two", " three"]
    spans = [(w.start, w.end) for w in seg.word_timings]
    assert spans == [(0.0, 0.5), (0.5, 1.5)]


def test_nudge_turn_end_trim_drops_words_beyond_the_new_end() -> None:
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())
    tid = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=10),
        end=BASE + timedelta(seconds=13),
        text="one two three",
        asr_model="m",
        word_timings=[
            Word(0.0, 1.0, " one", 1.0),
            Word(1.0, 2.0, " two", 1.0),
            Word(2.0, 3.0, " three", 1.0),
        ],
    )

    store.nudge_turn(tid, "end", -1.5)  # end moves -1.5s (into "two")

    seg = store.get_transcript(tid)
    assert seg is not None
    assert seg.word_timings is not None
    assert [w.text for w in seg.word_timings] == [" one", " two"]
    spans = [(w.start, w.end) for w in seg.word_timings]
    assert spans == [(0.0, 1.0), (1.0, 1.5)]  # " two" clipped at the new end


def test_nudge_turn_without_word_timings_still_moves_the_edge() -> None:
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())
    tid = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=10),
        end=BASE + timedelta(seconds=13),
        text="x",
        asr_model="m",
    )
    store.nudge_turn(tid, "start", 1.0)
    seg = store.get_transcript(tid)
    assert seg is not None
    assert seg.start == BASE + timedelta(seconds=11)
    assert seg.word_timings is None


def test_file_backed_store_uses_wal(tmp_path: Path) -> None:
    # WAL lets the six concurrent agents read while one writes; the default
    # rollback journal made readers block on every writer commit.
    store = Store.open(tmp_path / "recall.sqlite")
    mode = store._conn.execute("PRAGMA journal_mode").fetchone()[0]
    store.close()
    assert mode == "wal"


def test_migrate_retries_when_another_process_won_the_race(tmp_path: Path) -> None:
    """Two agents opening an outdated DB race the same migration step. The loser's
    executescript fails ("table ... already exists" — the winner committed between
    the loser's version read and its apply); it used to die and get relaunched by
    launchd. It must roll back, re-read the version, and settle."""
    db = tmp_path / "recall.sqlite"
    conn = sqlite3.connect(db)
    conn.executescript(f"BEGIN;\n{_MIGRATIONS[0]}\nPRAGMA user_version = 1;\nCOMMIT;")
    conn.close()

    loser = Store.connect(db)  # connect() skips migrate; version is 1

    real = loser._apply_migration
    fired = False

    def winner_beat_us(index: int) -> None:
        nonlocal fired
        if not fired:
            fired = True
            # The winner commits the FULL ladder in the loser's race window.
            other = Store.open(db)
            other.close()
            # ...so the loser's own attempt at the same step now blows up.
            msg = "table speakers already exists"
            raise sqlite3.OperationalError(msg)
        real(index)

    loser._apply_migration = winner_beat_us  # type: ignore[method-assign]
    loser.migrate()
    assert loser.schema_version() == SCHEMA_VERSION
    loser.close()


def test_day_summaries_round_trip_and_track_missing_days() -> None:
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,  # 2026-06-13
        end=BASE + timedelta(seconds=2),
        text="something was said",
        asr_model="m",
    )

    # A day with visible turns and no summary is missing; hidden-only days are not.
    assert store.days_missing_summaries(limit=10) == ["2026-06-13"]

    store.set_day_summary("2026-06-13", "A quiet day at home.", model="test-llm")
    assert store.get_day_summary("2026-06-13") == "A quiet day at home."
    assert store.days_missing_summaries(limit=10) == []

    # Regenerating overwrites (summaries are derived views, like transcripts).
    store.set_day_summary("2026-06-13", "Rewritten.", model="test-llm")
    assert store.get_day_summary("2026-06-13") == "Rewritten."
    assert store.get_day_summary("2026-01-01") is None


def test_recent_day_summaries_lists_newest_first() -> None:
    store = Store.memory()
    store.set_day_summary("2026-06-13", "first", model="m")
    store.set_day_summary("2026-06-15", "third", model="m")
    store.set_day_summary("2026-06-14", "second", model="m")
    got = store.recent_day_summaries(limit=2)
    assert [(d, t) for d, t, _ in got] == [
        ("2026-06-15", "third"),
        ("2026-06-14", "second"),
    ]


def test_vocabulary_terms_round_trip() -> None:
    store = Store.memory()
    a = store.add_vocabulary_term("Zutphen")
    store.add_vocabulary_term("EGA wing")
    store.add_vocabulary_term("Zutphen")  # duplicate is a no-op, not a second row
    assert [t.term for t in store.vocabulary_terms()] == ["EGA wing", "Zutphen"]
    store.delete_vocabulary_term(a)
    assert [t.term for t in store.vocabulary_terms()] == ["EGA wing"]


def test_vocabulary_rejects_blank_terms() -> None:
    store = Store.memory()
    with pytest.raises(ValueError, match="blank"):
        store.add_vocabulary_term("   ")


def test_migration_backfills_correction_audio_confidence(tmp_path: Path) -> None:
    """Corrections predating the audio_confidence column get it from their
    original turn, so quality-weighting has something to weight for old data."""
    db = tmp_path / "recall.sqlite"
    conn = sqlite3.connect(db)
    for index, step in enumerate(_MIGRATIONS[:22]):
        conn.executescript(
            f"BEGIN;\n{step}\nPRAGMA user_version = {index + 1};\nCOMMIT;"
        )
    conn.execute(
        """INSERT INTO transcript_segments
           (id, start_utc, end_utc, text, asr_model, asr_confidence)
           VALUES (1, '2026-06-13T12:00:00+00:00', '2026-06-13T12:00:03+00:00',
                   'orig', 'whisper', 0.42)"""
    )
    conn.execute(
        """INSERT INTO corrections
           (id, transcript_segment_id, start_utc, end_utc, original_text,
            corrected_text, created_utc)
           VALUES (1, 1, '2026-06-13T12:00:00+00:00', '2026-06-13T12:00:03+00:00',
                   'orig', 'fixed', '2026-06-13T13:00:00+00:00')"""
    )
    conn.commit()
    conn.close()

    store = Store.open(db)
    row = store._conn.execute(
        "SELECT audio_confidence FROM corrections WHERE id = 1"
    ).fetchone()
    store.close()
    assert row[0] == 0.42


def test_training_queue_skips_backchannels() -> None:
    # Labeling effort should go to contentful turns: sub-2s or few-word turns are
    # not offered by the queue (they're poor ASR training data). They stay fully
    # correctable from the timeline/session views.
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())

    def turn(at: float, dur: float, text: str) -> int:
        seg_id = store.add_transcript_segment(
            audio_segment_id=audio_id,
            start=BASE + timedelta(seconds=at),
            end=BASE + timedelta(seconds=at + dur),
            text=text,
            asr_model="whisper",
            asr_confidence=0.5,
        )
        store.set_loudness(seg_id, 0.05)
        return seg_id

    turn(0, 1.2, "Yeah okay good sure")  # too short
    turn(5, 4.0, "Yes. No.")  # too few words
    keeper = turn(10, 4.0, "the plumber is coming on Thursday")

    queued = store.training_queue(min_confidence=0.3, max_confidence=0.9)
    assert [t.id for t in queued] == [keeper]


def test_delete_source_removes_all_derived_rows_and_returns_paths() -> None:
    """Deleting an uploaded session must leave no orphans: its turns, corrections, and
    queued refine work all go, and the audio paths come back for the caller to unlink.
    """
    store = Store.memory()
    store.add_source(
        AudioSource(id="meeting-x", name="M", kind=SourceKind.UPLOAD, spec="")
    )
    audio_id = store.add_audio_segment(
        Segment(
            source_id="meeting-x",
            sequence=0,
            start=BASE,
            end=BASE + timedelta(minutes=5),
            path="/data/meeting-x/clip.mp3",
            sample_rate=48000,
            channels=1,
        )
    )
    turn_id = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=4),
        text="hello there",
        asr_model="whisper",
        asr_confidence=0.5,
    )
    store.add_correction(
        transcript_segment_id=turn_id,
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=4),
        original_text="hello there",
        corrected_text="Hello there.",
        language="en",
        created=BASE,
        speaker="Pippijn",
    )
    store.add_refine_request("meeting-x", BASE, BASE + timedelta(minutes=5))

    paths = store.delete_source("meeting-x")

    assert paths == ["/data/meeting-x/clip.mp3"]
    assert store.source_kind("meeting-x") is None
    assert store.audio_segment(audio_id) is None
    assert store.turns_by_id([turn_id]) == []
    assert store.pending_refine_requests() == []
    # a global voiceprint/speaker registry is untouched by a session delete
    conn = store._conn
    assert conn.execute("SELECT COUNT(*) FROM corrections").fetchone()[0] == 0


def test_live_summary_roundtrip_keyed_by_watermark() -> None:
    """The 'today so far' cache: one row, stamped with the day-state watermark it
    saw, so a request can tell fresh from stale without generating."""
    store = Store.memory()
    assert store.get_live_summary("2026-06-13") is None

    store.set_live_summary("2026-06-13", "Quiet morning.", model="m", watermark="a")
    row = store.get_live_summary("2026-06-13")
    assert row is not None
    assert row.text == "Quiet morning."
    assert row.watermark == "a"
    assert row.generated_utc  # stamped, so the UI can show "as of HH:MM"

    # Regeneration replaces the row (it's a cache, not a history).
    store.set_live_summary("2026-06-13", "Busy afternoon.", model="m", watermark="b")
    row = store.get_live_summary("2026-06-13")
    assert row is not None
    assert (row.text, row.watermark) == ("Busy afternoon.", "b")


def test_live_summary_keeps_only_the_current_day() -> None:
    """At UTC midnight the day key moves on; writing the new day evicts the old
    row so the table stays a single-purpose one-row cache."""
    store = Store.memory()
    store.set_live_summary("2026-06-13", "old day", model="m", watermark="a")
    store.set_live_summary("2026-06-14", "new day", model="m", watermark="b")
    assert store.get_live_summary("2026-06-13") is None
    assert store.get_live_summary("2026-06-14") is not None


def test_day_watermark_moves_on_every_visible_change() -> None:
    """The watermark must change whenever a regenerated summary could differ:
    a new turn, a HIDDEN turn, and — the bug that motivated it — a SPEAKER LABEL
    edit (labels update rows in place, so a max-id watermark missed them and
    annotation never reached the summary)."""
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())
    assert store.day_watermark("2026-06-13") is None

    first = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=2),
        text="hello there everyone",
        asr_model="whisper",
    )
    w1 = store.day_watermark("2026-06-13")
    assert w1 is not None
    assert store.day_watermark("2026-06-14") is None  # other days unaffected

    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=5),
        end=BASE + timedelta(seconds=8),
        text="more words arrived",
        asr_model="whisper",
    )
    w2 = store.day_watermark("2026-06-13")
    assert w2 != w1  # new turn moves it

    store.set_turn_speaker(first, "Alice")
    w3 = store.day_watermark("2026-06-13")
    assert w3 != w2  # labelling moves it (rows change in place, ids don't)

    store.set_turn_speaker(first, "Bob")
    w4 = store.day_watermark("2026-06-13")
    assert w4 != w3  # RE-labelling moves it too (same count, different name)

    store.hide(first, "junk")
    assert store.day_watermark("2026-06-13") != w4  # hiding moves it


def test_settings_roundtrip_and_overwrite() -> None:
    """Free-form settings (e.g. the household context given to the LLM) live in
    the DB, not the repo — the codebase stays PII-free; facts are data."""
    store = Store.memory()
    assert store.get_setting("household_context") is None
    store.set_setting("household_context", "Alice is left-handed.")
    assert store.get_setting("household_context") == "Alice is left-handed."
    store.set_setting("household_context", "Alice writes with her left hand.")
    assert store.get_setting("household_context") == "Alice writes with her left hand."
    # Clearing = storing empty; reads back as None so callers can `if context:`.
    store.set_setting("household_context", "  ")
    assert store.get_setting("household_context") is None


def test_a_hard_delete_journals_a_tombstone() -> None:
    # The deletion must cross the Isis split: the tombstone is what the Mac's sweep
    # pull is served from, and the veto that stops a later push resurrecting it.
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())

    assert store.is_tombstoned("usb", BASE) is False
    store.delete_audio_segments([audio_id])

    assert store.is_tombstoned("usb", BASE) is True
    (tomb,) = store.pending_sweeps()
    assert (tomb.source, tomb.start) == ("usb", BASE)
    # deleting twice is one fact, not two tombstones
    store.delete_audio_segments([audio_id])
    assert len(store.pending_sweeps()) == 1

    store.mark_sweep_done(tomb.id)
    assert store.pending_sweeps() == []
    assert store.is_tombstoned("usb", BASE) is True  # the veto outlives the sweep


def test_deleting_a_source_journals_every_segment() -> None:
    store = Store.memory()
    store.add_source(
        AudioSource(id="meeting-1", name="Meeting", kind=SourceKind.UPLOAD, spec="")
    )
    for offset in (0.0, 60.0):
        seg = _segment(offset)
        store.add_audio_segment(
            Segment(
                source_id="meeting-1",
                sequence=0,
                start=seg.start,
                end=seg.end,
                path=seg.path,
                sample_rate=48000,
                channels=1,
            )
        )
    store.delete_source("meeting-1")
    assert len(store.pending_sweeps()) == 2


def test_unmirrored_segments_are_the_processed_unstamped_ones() -> None:
    store = Store.memory()
    store.add_source(_source())
    unprocessed = store.add_audio_segment(_segment(0.0))
    processed = store.add_audio_segment(_segment(60.0))
    stamped = store.add_audio_segment(_segment(120.0))
    store.mark_transcribed(processed)
    store.mark_transcribed(stamped)
    store.mark_pushed(stamped)

    assert store.unmirrored_segments() == [processed]
    assert unprocessed  # the worker hasn't listened yet — not the mirror's turn

    # the doctor's in-flight slack: only segments processed before the cutoff count
    # (transcribed_utc is stamped with the segment's end time)
    assert store.unmirrored_segments(older_than=BASE + timedelta(seconds=61)) == []
    assert store.unmirrored_segments(older_than=datetime.now(UTC)) == [processed]


def test_audio_segment_id_at_resolves_the_cross_machine_identity() -> None:
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())
    assert store.audio_segment_id_at("usb", BASE) == audio_id
    assert store.audio_segment_id_at("usb", BASE + timedelta(seconds=1)) is None


def test_sweep_evidence_reports_the_macs_own_verdict_on_a_segment() -> None:
    # The Mac decides whether to honour a fleet sweep from its OWN database, not the
    # fleet's word: kind, its VAD verdict, and whether a visible turn survives.
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())

    # Never measured, no turn: kind is captured, but speech_s is still unknown here.
    e = store.sweep_evidence("usb", BASE)
    assert e is not None
    assert e.audio_id == audio_id
    assert e.kind == SourceKind.COREAUDIO
    assert e.speech_s is None
    assert e.has_speech is False

    # Scored speechless: now the sweep bar is cleared.
    store.set_audio_analysis(audio_id, speech_s=0.0, structure=None)
    e = store.sweep_evidence("usb", BASE)
    assert e is not None and e.speech_s == 0.0

    # A surviving turn flips has_speech, however quiet the mean looked.
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=1),
        text="not idle after all",
        asr_model="m",
    )
    e = store.sweep_evidence("usb", BASE)
    assert e is not None and e.has_speech is True


def test_sweep_evidence_is_none_when_the_segment_is_not_held() -> None:
    store = Store.memory()
    store.add_source(_source())
    store.add_audio_segment(_segment())
    assert store.sweep_evidence("usb", BASE + timedelta(seconds=1)) is None


def test_sweep_refusals_are_journaled_once_per_identity_and_counted() -> None:
    # The doctor's tamper gauge: a refused fleet sweep is kept audio, recorded so it
    # surfaces; re-serving the same tombstone must not multiply the count.
    store = Store.memory()
    assert store.sweep_refusal_count() == 0
    store.record_sweep_refusal("usb", BASE, "the Mac's VAD measured 4.2s of speech")
    store.record_sweep_refusal("usb", BASE, "re-served next pass")  # same identity
    store.record_sweep_refusal("usb", BASE + timedelta(seconds=60), "another")
    assert store.sweep_refusal_count() == 2
