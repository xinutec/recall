"""Rendering a session list / one session's transcript for CLI review."""

from __future__ import annotations

import json
from datetime import UTC, datetime, timedelta

from recall.ids import TranscriptId
from recall.sources import AudioSource, SourceKind
from recall.store import Store, TranscriptSegment
from recall.timeline import Segment
from recall.transcript_view import (
    attribution,
    clean_transcript,
    format_conversations,
    format_sessions,
    format_transcript,
    format_turn_details,
)

BASE = datetime(2026, 6, 22, 10, 33, 0, tzinfo=UTC)


def _session(rows: list[tuple[str, str | None, str | None]]) -> Store:
    """A session whose turns are (text, speaker_label, cluster)."""
    store = Store.memory()
    store.add_source(
        AudioSource(id="m", name="Meeting", kind=SourceKind.UPLOAD, spec="")
    )
    audio = store.add_audio_segment(
        Segment(
            source_id="m",
            sequence=0,
            start=BASE,
            end=BASE + timedelta(minutes=5),
            path="x",
            sample_rate=48000,
            channels=1,
        )
    )
    for i, (text, label, cluster) in enumerate(rows):
        store.add_transcript_segment(
            audio_segment_id=audio,
            start=BASE + timedelta(seconds=i * 10),
            end=BASE + timedelta(seconds=i * 10 + 5),
            text=text,
            asr_model="diarized",
            speaker_label=label,
            speaker_cluster=cluster,
        )
    return store


def test_transcript_attributes_each_turn_with_speaker_and_time() -> None:
    store = _session(
        [
            ("Hello there.", "Pippijn", "SPEAKER_01"),
            ("How are you?", "Dr. Adams", "SPEAKER_00"),
            ("Mm.", None, "SPEAKER_01"),  # unnamed → falls back to the voice
        ]
    )
    out = format_transcript("m", store.session_turns("m"))
    assert "Pippijn: Hello there." in out
    assert "Dr. Adams: How are you?" in out
    assert "SPEAKER_01: Mm." in out  # unnamed → falls back to the voice


def test_transcript_shows_local_wall_clock_time() -> None:
    store = _session([("Hi.", "Pippijn", "SPEAKER_01")])
    out = format_transcript("m", store.session_turns("m"))
    # Times are stored UTC but shown in the local zone the call happened on.
    assert f"[{BASE.astimezone():%H:%M:%S}]" in out


def test_conversation_list_numbers_and_previews() -> None:
    later = BASE + timedelta(hours=4)
    rows = [
        (1, BASE, BASE + timedelta(minutes=2), 29, "Yes, this is the lab"),
        (2, later, later + timedelta(minutes=7), 56, "Hello"),
    ]
    out = format_conversations("today", rows)
    assert "today — 2 conversation(s)" in out
    assert "1. " in out
    assert "Yes, this is the lab" in out
    assert "29 turns" in out


def test_transcript_json_is_clean_and_coalesced() -> None:
    store = _session(
        [
            ("Hi.", "Pippijn", "SPEAKER_01"),
            ("How are you?", "Pippijn", "SPEAKER_01"),
            ("Fine.", "Dr. Adams", "SPEAKER_00"),
        ]
    )
    data = json.loads(format_transcript("m", store.session_turns("m"), as_json=True))
    assert data["session"] == "m"
    assert data["speakers"] == ["Pippijn", "Dr. Adams"]
    # consecutive same-speaker turns merge into one bubble
    assert [t["speaker"] for t in data["turns"]] == ["Pippijn", "Dr. Adams"]
    assert data["turns"][0]["text"] == "Hi. How are you?"
    assert data["turns"][1]["text"] == "Fine."
    assert data["date"] == data["turns"][0]["start"]  # first bubble's start


def test_clean_transcript_coalesces_and_labels_unconfirmed() -> None:
    store = _session(
        [
            ("Hi.", None, "SPEAKER_01"),
            ("Yo.", None, "SPEAKER_00"),
            ("Bye.", None, "SPEAKER_00"),
        ]
    )
    out = clean_transcript("m", store.session_turns("m"))
    # unconfirmed turns fall back to the diarization voice; distinct voices stay apart
    assert [t["speaker"] for t in out["turns"]] == ["SPEAKER_01", "SPEAKER_00"]
    assert out["turns"][1]["text"] == "Yo. Bye."  # same voice merged
    assert out["speakers"] == []  # none confirmed
    assert clean_transcript("m", store.session_turns("m")) == out  # deterministic


def test_clean_transcript_start_is_iso_with_local_offset() -> None:
    store = _session([("Hi.", "Pippijn", "SPEAKER_01")])
    out = clean_transcript("m", store.session_turns("m"))
    parsed = datetime.fromisoformat(out["turns"][0]["start"])
    assert parsed.utcoffset() is not None  # carries a local offset
    assert parsed == BASE  # same instant as the stored UTC start


def test_session_list_shows_id_count_and_speakers() -> None:
    store = _session([("a", "Pippijn", "S1"), ("b", "Dr. Adams", "S0")])
    out = format_sessions(store.session_summaries())
    assert "m" in out
    assert "2 turns" in out
    assert "Pippijn" in out


def test_session_list_json_round_trips() -> None:
    store = _session([("a", "Pippijn", "S1")])
    data = json.loads(format_sessions(store.session_summaries(), as_json=True))
    assert data[0]["id"] == "m"
    assert data[0]["turns"] == 1


def test_empty_session_list() -> None:
    assert format_sessions([]) == "no sessions recorded"


def _turn(  # noqa: PLR0913 - one kwarg per field, for terse test construction
    *,
    label: str | None = None,
    guess: str | None = None,
    score: float | None = None,
    cluster: str | None = None,
    tid: int = 1,
    text: str = "t",
    conf: float | None = None,
    loudness: float | None = None,
    source: str | None = None,
    superseded: int | None = None,
    provenance: str | None = None,
    end: datetime | None = None,
) -> TranscriptSegment:
    return TranscriptSegment(
        id=TranscriptId(tid),
        audio_segment_id=None,
        start=BASE,
        end=end if end is not None else BASE,
        text=text,
        language="en",
        language_confidence=None,
        asr_confidence=conf,
        asr_model="m",
        speaker_label=label,
        speaker_id=None,
        superseded_by=TranscriptId(superseded) if superseded is not None else None,
        provenance=provenance,
        loudness=loudness,
        speaker_guess=guess,
        speaker_score=score,
        speaker_cluster=cluster,
        source_id=source,
    )


def test_attribution_confirmed_name_is_authoritative() -> None:
    # A human-confirmed label wins outright — even over a strong voiceprint guess.
    assert attribution(_turn(label="Alice", guess="Bob", score=0.9)) == "Alice"


def test_attribution_shows_guess_with_match_strength() -> None:
    # No confirmed label: surface the voiceprint guess + its rounded score, as a hint.
    assert attribution(_turn(guess="Pippijn", score=0.764)) == "Pippijn ~76%"


def test_attribution_guess_without_score() -> None:
    assert attribution(_turn(guess="Pippijn")) == "Pippijn"


def test_attribution_falls_back_to_cluster_then_unknown() -> None:
    assert attribution(_turn(cluster="SPEAKER_00")) == "SPEAKER_00"
    assert attribution(_turn()) == "unknown"


def test_turn_details_renders_the_diagnostic_fields() -> None:
    out = format_turn_details(
        [
            _turn(
                tid=39110,
                text="less veins here now",
                conf=0.9254,
                loudness=0.00052,
                guess="Pippijn",
                score=0.70,
                cluster="SPEAKER_00",
                source="usb",
                provenance="diarized-aligned (m)",
                end=BASE + timedelta(seconds=6),
            )
        ]
    )
    assert "#39110" in out
    assert "0.93" in out  # conf, shown to 2dp
    assert "faint" in out  # loudness 0.0005 -> faint
    assert "Pippijn ~70% (guess)" in out  # attribution + qualifier
    assert "SPEAKER_00" in out
    assert "src=usb" in out
    assert "less veins here now" in out


def test_turn_details_marks_confirmed_and_superseded() -> None:
    out = format_turn_details(
        [_turn(tid=1, label="Alice", text="hi"), _turn(tid=2, text="old", superseded=5)]
    )
    assert "Alice (confirmed)" in out
    assert "superseded by #5" in out
