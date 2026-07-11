"""Mac-side sync push: only changed segments go over the wire, and the audio blob is
uploaded once. The watermark must catch both new segments and refine re-derivations."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from pathlib import Path

from recall.sources import AudioSource, SourceKind
from recall.store import Store
from recall.sync import SegmentIn, SegmentStoredOut, SummaryIn
from recall.sync_push import sync_push
from recall.timeline import Segment

BASE = datetime(2026, 7, 11, 12, 0, 0, tzinfo=UTC)


class FakeClient:
    """Records what the push sent, and pretends the fleet holds nothing at first."""

    def __init__(self) -> None:
        self.present: set[tuple[str, str]] = set()
        self.audio_pushed: list[tuple[str, str]] = []
        self.segments: list[SegmentIn] = []
        self.summaries: list[SummaryIn] = []

    def audio_present(self, source: str, name: str) -> bool:
        return (source, name) in self.present

    def push_audio(self, source: str, name: str, local_path: Path) -> bool:
        self.audio_pushed.append((source, name))
        self.present.add((source, name))
        return True

    def push_segment(self, segment: SegmentIn) -> SegmentStoredOut:
        self.segments.append(segment)
        return SegmentStoredOut(audio_segment_id=len(self.segments), turns_written=1)

    def push_summary(self, summary: SummaryIn) -> None:
        self.summaries.append(summary)


def _seed_segment(store: Store, model: str = "turbo") -> int:
    store.add_source(
        AudioSource(id="usb", name="Household", kind=SourceKind.COREAUDIO, spec="")
    )
    audio_id = store.add_audio_segment(
        Segment(
            source_id="usb",
            sequence=0,
            start=BASE,
            end=BASE + timedelta(seconds=30),
            path="/archive/usb/seg-0001.opus",
            sample_rate=48000,
            channels=1,
        )
    )
    store.add_transcript_segment(
        audio_segment_id=int(audio_id),
        start=BASE,
        end=BASE + timedelta(seconds=5),
        text="hello",
        asr_model=model,
    )
    return int(audio_id)


def test_push_sends_new_segment_and_audio_then_is_idempotent() -> None:
    store = Store.memory()
    _seed_segment(store)

    client = FakeClient()
    assert sync_push(store, client) == 1
    assert client.audio_pushed == [("usb", "seg-0001.opus")]
    assert len(client.segments) == 1
    assert client.segments[0].source_name == "Household"  # real source name, not the id

    # second pass: nothing changed → nothing pushed, and no re-upload of the blob
    assert sync_push(store, client) == 0
    assert client.audio_pushed == [("usb", "seg-0001.opus")]


def test_push_resends_a_re_derived_segment_but_not_its_audio() -> None:
    store = Store.memory()
    audio_id = _seed_segment(store)
    client = FakeClient()
    sync_push(store, client)  # initial push sets the watermark

    # a refine re-derivation mints a new turn (higher id) for the same segment
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=6),
        text="hello there",
        asr_model="adapter",
    )
    assert sync_push(store, client) == 1  # the changed segment is pushed again
    # …but the immutable audio is already on the fleet, so it isn't re-uploaded
    assert client.audio_pushed == [("usb", "seg-0001.opus")]
    assert len(client.segments) == 2


def test_push_sends_day_summaries() -> None:
    store = Store.memory()
    store.set_day_summary("2026-07-11", "a quiet day", model="qwen")
    client = FakeClient()
    sync_push(store, client)
    assert [(s.day, s.text) for s in client.summaries] == [
        ("2026-07-11", "a quiet day")
    ]
