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


def test_diarize_picker_prefers_segments_with_more_transcribed_speech() -> None:
    # Newest-first alone sent the daemon at the quiet-night junk tail (short
    # hallucinations on near-silent audio, newest ids) while a visit's dense
    # conversation (older ids, lots of text) waited hours (#1331, measured
    # 2026-09-03 overnight). The picker now weights by how much visible speech a
    # segment already carries, so the substantial audio is refined first and the
    # thin tail sorts to the back; recency is only the tiebreak.
    store = Store.memory()
    store.add_source(_source())
    dense = store.add_audio_segment(_segment(0))  # OLDER id
    thin = store.add_audio_segment(_segment(120))  # NEWER id
    store.add_transcript_segment(
        audio_segment_id=dense,
        start=BASE,
        end=BASE + timedelta(seconds=3),
        text="a long stretch of real household conversation worth attributing",
        asr_model="m",
    )
    store.add_transcript_segment(
        audio_segment_id=thin,
        start=BASE + timedelta(seconds=120),
        end=BASE + timedelta(seconds=121),
        text="uh",
        asr_model="m",
    )
    # Dense first despite its older id; newest-first would have returned thin first.
    assert store.audio_segments_to_diarize(limit=10) == [dense, thin]


def test_diarize_picker_breaks_ties_by_recency() -> None:
    # Equal speech weight → the more recent segment still wins, so among comparable
    # candidates the freshest audio is refined first (the good half of newest-first).
    store = Store.memory()
    store.add_source(_source())
    older = store.add_audio_segment(_segment(0))
    newer = store.add_audio_segment(_segment(120))
    for aid, start in ((older, BASE), (newer, BASE + timedelta(seconds=120))):
        store.add_transcript_segment(
            audio_segment_id=aid,
            start=start,
            end=start + timedelta(seconds=2),
            text="same length here",
            asr_model="m",
        )
    assert store.audio_segments_to_diarize(limit=10) == [newer, older]


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


def test_unreadable_capture_is_recorded_once_and_then_known() -> None:
    # An unreadable capture file is recorded once (so the caller logs it once) and then
    # listed as known (so the scan skips re-probing it) — the fix for the re-probe loop.
    store = Store.memory()
    assert store.unreadable_capture_names("usb") == set()
    assert store.mark_unreadable_capture("usb", "usb-20260627T135734.opus") is True
    assert store.mark_unreadable_capture("usb", "usb-20260627T135734.opus") is False
    assert store.unreadable_capture_names("usb") == {"usb-20260627T135734.opus"}
    # scoped per source
    assert store.mark_unreadable_capture("pixel9", "pixel9-x.opus") is True
    assert store.unreadable_capture_names("usb") == {"usb-20260627T135734.opus"}


def test_rollback_recovers_a_connection_wedged_by_a_failed_write(
    tmp_path: Path,
) -> None:
    # A write that fails under lock contention leaves the connection with an aborted
    # transaction open, which in WAL mode freezes its read snapshot — so a long-lived
    # daemon stops seeing rows other connections commit (the bug that hung Ask).
    # store.rollback() must clear that state and restore fresh reads.
    db = tmp_path / "recall.sqlite"
    a = Store.open(db)
    a.add_source(_source())
    b = Store.open(db)  # the "daemon" connection

    # b's write is blocked by a holds the write lock, so it fails and leaves b wedged.
    a._conn.execute("BEGIN IMMEDIATE")
    a._conn.execute("INSERT INTO settings(key, value) VALUES ('x', '1')")
    b._conn.execute("PRAGMA busy_timeout = 200")
    with pytest.raises(sqlite3.OperationalError):
        b.set_setting("y", "2")  # blocked → busy timeout → raises, txn left open
    # Read into a local: asserting on `b._conn.in_transaction` directly pins it to
    # Literal[True] in mypy's binder, and it can't see that b.rollback() below clears
    # it — so the post-rollback assert would look always-false (unreachable).
    wedged = b._conn.in_transaction
    assert wedged  # an aborted transaction is still open
    a._conn.rollback()  # the other writer releases the lock

    # Another connection commits a NEW row while b is wedged.
    a.add_refine_request("usb", BASE, BASE + timedelta(seconds=60))

    b.rollback()  # recover
    recovered = b._conn.in_transaction
    assert not recovered
    assert len(b.pending_refine_requests(limit=10)) == 1  # b sees the fresh row
    a.close()
    b.close()


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

    store.name_voice("usb", "SPEAKER_00", "Dr. Voss")
    store.name_voice("usb", "SPEAKER_01", "Pippijn")
    namings = {(n.source_id, n.cluster): n.name for n in store.cluster_namings()}
    assert namings == {
        ("usb", "SPEAKER_00"): "Dr. Voss",
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
    # The tombstone is the whole of what a deletion travels as now: the veto that
    # stops a later push resurrecting it. It is a record, not an order — nothing
    # serves it to a recorder (docs/architecture.md, "Deletion authority").
    store = Store.memory()
    store.add_source(_source())
    audio_id = store.add_audio_segment(_segment())

    assert store.is_tombstoned("usb", BASE) is False
    store.delete_audio_segments([audio_id])
    assert store.is_tombstoned("usb", BASE) is True

    # deleting twice is one fact, not two tombstones
    store.delete_audio_segments([audio_id])
    assert store.is_tombstoned("usb", BASE) is True


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
    assert store.is_tombstoned("meeting-1", BASE) is True
    assert store.is_tombstoned("meeting-1", BASE + timedelta(seconds=60)) is True


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


def test_register_source_corrects_a_guessed_kind_but_keeps_a_human_name() -> None:
    # The worker registers what it discovers on disk; the agent that actually produces
    # the audio corrects the kind when it starts. A name the user chose in the UI is
    # theirs, though — re-registering a phone must not rename it back to its id.
    store = Store.memory()
    store.add_source(
        AudioSource(id="pixel9", name="pixel9", kind=SourceKind.DISCOVERED, spec="")
    )
    store.rename_source("pixel9", "Kitchen")
    store.register_source(
        AudioSource(id="pixel9", name="pixel9", kind=SourceKind.TCP_PCM, spec="")
    )
    assert store.source_kind("pixel9") is SourceKind.TCP_PCM
    src = store.source("pixel9")
    assert src is not None and src.name == "Kitchen"


def test_register_source_replaces_a_placeholder_name() -> None:
    # A name equal to the id is the worker's placeholder, not a choice — the registrar
    # that knows what this is may say so. Without this an upload the worker discovered
    # first stays listed as "meeting-20260731-0916" instead of "Meeting …".
    store = Store.memory()
    store.add_source(
        AudioSource(
            id="meeting-x", name="meeting-x", kind=SourceKind.DISCOVERED, spec=""
        )
    )
    store.register_source(
        AudioSource(
            id="meeting-x",
            name="Meeting 2026-07-31 09:16",
            kind=SourceKind.UPLOAD,
            spec="",
        )
    )
    src = store.source("meeting-x")
    assert src is not None and src.name == "Meeting 2026-07-31 09:16"
