"""Span assignment: the one gesture behind reassign / split / merge."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta

from recall.asr import Word
from recall.conversation import (
    _MIN_PIECE,
    _min_width_pieces,
    _Piece,
    _recut,
    _snap_to_word,
    assign_span,
)
from recall.sources import AudioSource, SourceKind
from recall.store import Store
from recall.timeline import Segment

BASE = datetime(2026, 6, 13, 12, 0, 0, tzinfo=UTC)
NOW = datetime(2026, 6, 13, 16, 0, 0, tzinfo=UTC)

# "I have already made a list of errands" — non-uniform timing: the first four words
# are quick (done by 0.5s), so the word boundary at "a" (0.5s) is far from where char
# interpolation would put it (char 20/37 → ~1.6s of a 3s turn).
_SPLIT_TEXT = "I have already made a list of errands"
_SPLIT_WORDS = [
    Word(0.0, 0.06, "I", 0.9),
    Word(0.06, 0.12, " have", 0.9),
    Word(0.12, 0.3, " already", 0.9),
    Word(0.3, 0.5, " made", 0.9),
    Word(0.5, 0.7, " a", 0.9),
    Word(0.7, 1.2, " list", 0.9),
    Word(1.2, 1.5, " of", 0.9),
    Word(1.5, 3.0, " errands", 0.9),
]


def _one_turn(*, words: list[Word] | None) -> tuple[Store, int]:
    store = Store.memory()
    store.add_source(AudioSource(id="m", name="M", kind=SourceKind.UPLOAD, spec=""))
    audio_id = store.add_audio_segment(
        Segment(
            source_id="m",
            sequence=0,
            start=BASE,
            end=BASE + timedelta(seconds=3),
            path="x",
            sample_rate=48000,
            channels=1,
        )
    )
    tid = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=3),
        text=_SPLIT_TEXT,
        asr_model="diarized",
        speaker_label="Pippijn",
        speaker_cluster="C",
        word_timings=words,
    )
    return store, int(tid)


def _store_with_turns(rows: list[tuple[str, str]]) -> tuple[Store, list[int]]:
    store = Store.memory()
    store.add_source(AudioSource(id="m", name="M", kind=SourceKind.UPLOAD, spec=""))
    audio_id = store.add_audio_segment(
        Segment(
            source_id="m",
            sequence=0,
            start=BASE,
            end=BASE + timedelta(seconds=300),
            path="x",
            sample_rate=48000,
            channels=1,
        )
    )
    ids = [
        store.add_transcript_segment(
            audio_segment_id=audio_id,
            start=BASE + timedelta(seconds=i * 10),
            end=BASE + timedelta(seconds=i * 10 + 10),
            text=text,
            asr_model="diarized",
            speaker_label=spk,
            speaker_cluster="C",
        )
        for i, (text, spk) in enumerate(rows)
    ]
    return store, [int(i) for i in ids]


def _current(store: Store) -> list[tuple[str, str | None]]:
    rows = store.segments_in_range(BASE, BASE + timedelta(seconds=400))
    return [(r.text, r.speaker_label) for r in rows]


def test_assign_within_a_turn_splits_out_the_selected_part() -> None:
    # A turn that merged two speakers: the tail is actually the doctor.
    text = "we discussed it do you feel feverish"
    store, ids = _store_with_turns([(text, "Pippijn")])
    lo = text.index("do you")
    n = assign_span(store, "m", ids[0], lo, ids[0], len(text), "Dr Adams", now=NOW)
    assert n == 1
    assert _current(store) == [
        ("we discussed it", "Pippijn"),
        ("do you feel feverish", "Dr Adams"),
    ]


def test_assign_across_turns_splits_both_edges_and_relabels_the_middle() -> None:
    store, ids = _store_with_turns(
        [
            ("hello there how are you", "Pippijn"),
            ("I am the doctor", "Pippijn"),
            ("fine thanks and you", "Pippijn"),
        ]
    )
    start = "hello there how are you"
    end = "fine thanks and you"
    assign_span(
        store,
        "m",
        ids[0],
        start.index("how"),
        ids[2],
        end.index(" and"),
        "Dr Adams",
        now=NOW,
    )
    texts = _current(store)
    assert ("hello there", "Pippijn") in texts
    assert ("how are you", "Dr Adams") in texts
    assert ("I am the doctor", "Dr Adams") in texts  # whole middle turn relabelled
    assert ("fine thanks", "Dr Adams") in texts
    assert ("and you", "Pippijn") in texts


def test_split_with_word_timings_snaps_to_the_real_word_boundary() -> None:
    store, tid = _one_turn(words=_SPLIT_WORDS)
    lo = _SPLIT_TEXT.index("a list")  # move "a list of errands" to the doctor
    assign_span(store, "m", tid, lo, tid, len(_SPLIT_TEXT), "Dr Adams", now=NOW)

    by_text = {
        r.text: r for r in store.segments_in_range(BASE, BASE + timedelta(seconds=10))
    }
    moved = by_text["a list of errands"]
    assert moved.speaker_label == "Dr Adams"
    # Snapped to the START of the word "a" (0.5s) — NOT char interpolation (~1.5s).
    assert moved.start == BASE + timedelta(seconds=0.5)
    assert by_text["I have already made"].end == BASE + timedelta(seconds=0.5)
    # The moved piece carries its own word timings, re-based to its start (both edges,
    # so the start AND end of each word shift by the piece's offset).
    assert moved.word_timings is not None
    assert [w.text for w in moved.word_timings] == [" a", " list", " of", " errands"]
    rebased = [(round(w.start, 4), round(w.end, 4)) for w in moved.word_timings]
    assert rebased == [(0.0, 0.2), (0.2, 0.7), (0.7, 1.0), (1.0, 2.5)]


def test_split_without_word_timings_interpolates_by_char() -> None:
    store, tid = _one_turn(words=None)
    lo = _SPLIT_TEXT.index("a list")
    assign_span(store, "m", tid, lo, tid, len(_SPLIT_TEXT), "Dr Adams", now=NOW)
    moved = next(
        r
        for r in store.segments_in_range(BASE, BASE + timedelta(seconds=10))
        if r.text == "a list of errands"
    )
    # No word timings → interpolation by character position (the estimate).
    assert moved.start == BASE + timedelta(seconds=3.0 * lo / len(_SPLIT_TEXT))
    assert moved.word_timings is None


def test_split_anchors_edge_pieces_to_the_turn_not_the_word_timestamps() -> None:
    # Word timestamps can sit inside the turn (silence before the first word, and after
    # the last). The split's outer edges anchor to the turn's own start/end, not the
    # word times — else opening/closing audio is dropped. The interior cut still snaps
    # to a word boundary.
    text = "hello world"
    # words occupy [1.0, 2.0]; the turn runs [0, 3] — 1s of silence on each side.
    words = [Word(1.0, 1.5, " hello", 0.9), Word(1.5, 2.0, " world", 0.9)]
    store = Store.memory()
    store.add_source(AudioSource(id="m", name="M", kind=SourceKind.UPLOAD, spec=""))
    audio_id = store.add_audio_segment(
        Segment(
            source_id="m",
            sequence=0,
            start=BASE,
            end=BASE + timedelta(seconds=3),
            path="x",
            sample_rate=48000,
            channels=1,
        )
    )
    tid = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=3),
        text=text,
        asr_model="diarized",
        speaker_label="Pippijn",
        speaker_cluster="C",
        word_timings=words,
    )
    lo = text.index("world")
    assign_span(store, "m", int(tid), lo, int(tid), len(text), "Dr", now=NOW)
    by_text = {
        r.text: r for r in store.segments_in_range(BASE, BASE + timedelta(seconds=10))
    }
    # First piece starts at the turn start (BASE), not the word's 1.0s; interior cut
    # snaps to the word boundary at 1.5s; last piece ends at the turn end (3s), not 2.0.
    assert by_text["hello"].start == BASE
    assert by_text["hello"].end == BASE + timedelta(seconds=1.5)
    assert by_text["world"].start == BASE + timedelta(seconds=1.5)
    assert by_text["world"].end == BASE + timedelta(seconds=3)


def test_split_of_a_diarized_turn_keeps_the_pieces_diarized() -> None:
    # Splitting a diarized turn must not downgrade the pieces to a lower tier — else the
    # session sees a non-diarized turn and drops out of its annotation view. The pieces'
    # provenance keeps the diarized marker.
    store = Store.memory()
    store.add_source(AudioSource(id="m", name="M", kind=SourceKind.UPLOAD, spec=""))
    audio_id = store.add_audio_segment(
        Segment(
            source_id="m",
            sequence=0,
            start=BASE,
            end=BASE + timedelta(seconds=3),
            path="x",
            sample_rate=48000,
            channels=1,
        )
    )
    tid = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=2),
        text="a list of errands",
        asr_model="whisper",
        provenance="diarized-aligned (whisper)",
        speaker_label="Dr Lee",
        speaker_cluster="C",
    )
    assign_span(store, "m", int(tid), 0, int(tid), len("a list"), "Pippijn", now=NOW)
    pieces = store.segments_in_range(BASE, BASE + timedelta(seconds=10))
    assert pieces  # the split happened
    assert all((p.provenance or "").startswith("diarized") for p in pieces), [
        p.provenance for p in pieces
    ]


def test_split_never_creates_a_zero_width_piece() -> None:
    # Degenerate word timings (a word collapsed to zero duration, as a bad alignment can
    # produce) must not yield a zero-length, audio-less turn: every piece gets a minimum
    # playable span.
    words = [
        Word(0.0, 0.0, " Painting", 1.0),  # collapsed — no audio in this span
        Word(0.0, 0.0, " and", 1.0),
        Word(0.0, 2.0, " Thursday", 1.0),
    ]
    store = Store.memory()
    store.add_source(AudioSource(id="m", name="M", kind=SourceKind.UPLOAD, spec=""))
    audio_id = store.add_audio_segment(
        Segment(
            source_id="m",
            sequence=0,
            start=BASE,
            end=BASE + timedelta(seconds=3),
            path="x",
            sample_rate=48000,
            channels=1,
        )
    )
    tid = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=2),
        text="Painting and Thursday",
        asr_model="human",
        speaker_label="Dr",
        speaker_cluster="C",
        word_timings=words,
    )
    assign_span(store, "m", int(tid), 0, int(tid), len("Painting"), "Pippijn", now=NOW)
    pieces = store.segments_in_range(BASE, BASE + timedelta(seconds=10))
    by_text = {p.text: p for p in pieces}
    assert set(by_text) == {"Painting", "and Thursday"}
    # The collapsed first piece is widened to exactly _MIN_PIECE; the rest follows it.
    assert by_text["Painting"].start == BASE
    assert by_text["Painting"].end == BASE + _MIN_PIECE
    assert by_text["and Thursday"].start == BASE + _MIN_PIECE
    assert by_text["and Thursday"].end == BASE + timedelta(seconds=2)


def test_snap_to_word_moves_a_mid_word_cut_to_the_nearest_boundary() -> None:
    # On the no-word-timings split path, a cut inside a word snaps to the nearest space
    # (ties go left); a cut already on a boundary, or in a word with none, is unchanged.
    text = "hello world there"  # spaces at 5 and 11; len 17
    assert _snap_to_word(text, 1) == 5  # near the start of "hello" → right
    assert _snap_to_word(text, 3) == 5  # in "hello", no space left → right
    assert _snap_to_word(text, 6) == 6  # just after a space → already a boundary
    assert _snap_to_word(text, 8) == 5  # in "world", equidistant 5/11 → left
    assert _snap_to_word(text, 9) == 11  # in "world", 11 is nearer → right
    assert _snap_to_word(text, 14) == 11  # in "there", no space right → left
    assert _snap_to_word(text, 5) == 5  # already on the space → unchanged
    assert _snap_to_word("solid", 2) == 2  # one word, no spaces → unchanged
    assert _snap_to_word(text, -3) == 0  # clamped into [0, len]
    assert _snap_to_word(text, 100) == len(text)


def test_min_width_pieces_pulls_back_when_out_of_room() -> None:
    # Defensive branch: when cumulative minimum widths exceed the turn, the last piece
    # is pulled back so it stays a valid (non-inverted) span at the turn end.
    start, end = BASE, BASE + timedelta(seconds=0.08)
    pieces: list[_Piece] = [
        (start, start, "a", "X", None),
        (start, start, "b", "Y", None),
        (start, end, "c", "Z", None),
    ]
    out = _min_width_pieces(pieces, start, end)
    # every piece is valid (non-inverted) and within the turn
    assert all(s < e and start <= s and e <= end for s, e, *_ in out)
    # the crammed last piece is pulled back by exactly _MIN_PIECE from the turn end
    assert (out[-1][0], out[-1][1]) == (end - _MIN_PIECE, end)


def test_recut_claims_the_turn_so_a_concurrent_resplit_makes_no_duplicates() -> None:
    # An impatient double-tap fires two splits of the same turn at once; both read it as
    # live and reach _recut. The atomic claim lets only the first split it — the second
    # is a no-op, so the line isn't stamped with duplicate sets of pieces.
    store, tid = _one_turn(words=_SPLIT_WORDS)
    cut = _SPLIT_TEXT.index("a list")
    assert _recut(store, tid, [cut], ["Pippijn", "Dr"], now=NOW) == 1
    # The turn is now claimed (hidden); a second _recut on the same id does nothing.
    assert _recut(store, tid, [cut], ["Pippijn", "Dr"], now=NOW) == 0
    pieces = [
        r
        for r in store.segments_in_range(BASE, BASE + timedelta(seconds=10))
        if (r.provenance or "").endswith(f"split of #{tid}")
    ]
    assert len(pieces) == 2  # exactly one set of pieces, not doubled


def test_assign_a_whole_turn_just_relabels_it() -> None:
    store, ids = _store_with_turns([("hello there", "Pippijn")])
    n = assign_span(
        store, "m", ids[0], 0, ids[0], len("hello there"), "Dr Adams", now=NOW
    )
    assert n == 1
    assert _current(store) == [("hello there", "Dr Adams")]
