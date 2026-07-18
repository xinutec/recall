"""Mac-side sync push: only changed segments go over the wire, and the audio blob is
uploaded once. The watermark must catch both new segments and refine re-derivations."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import override

import httpx
import pytest

from recall.sources import AudioSource, SourceKind
from recall.store import Store
from recall.sync import LabelOut, SegmentIn, SegmentStoredOut, SummaryIn, TurnIn
from recall.sync_push import pull_labels, push_live_turns, sync_push
from recall.timeline import Segment

BASE = datetime(2026, 7, 11, 12, 0, 0, tzinfo=UTC)


class FakeClient:
    """Records what the push sent, and pretends the fleet holds nothing at first."""

    def __init__(self) -> None:
        self.present: set[tuple[str, str]] = set()
        self.audio_pushed: list[tuple[str, str]] = []
        self.segments: list[SegmentIn] = []
        self.summaries: list[SummaryIn] = []
        self.live: list[TurnIn] = []
        # What the fleet would return on GET /sync/labels.
        self.labels: list[LabelOut] = []

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

    def push_live(self, turns: list[TurnIn]) -> int:
        self.live.extend(turns)
        return len(turns)

    def fetch_labels(self) -> list[LabelOut]:
        return list(self.labels)


def _blob(root: Path, name: str = "seg-0001.opus") -> Path:
    """A real file on disk — the push checks existence before sending."""
    path = root / "usb" / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(b"opus-bytes")
    return path


def _seed_segment(
    store: Store,
    root: Path,
    model: str | None = "turbo",
    *,
    name: str = "seg-0001.opus",
    start: datetime = BASE,
) -> int:
    store.add_source(
        AudioSource(id="usb", name="Household", kind=SourceKind.COREAUDIO, spec="")
    )
    audio_id = store.add_audio_segment(
        Segment(
            source_id="usb",
            sequence=0,
            start=start,
            end=start + timedelta(seconds=30),
            path=str(_blob(root, name)),
            sample_rate=48000,
            channels=1,
        )
    )
    if model is not None:
        store.add_transcript_segment(
            audio_segment_id=int(audio_id),
            start=start,
            end=start + timedelta(seconds=5),
            text="hello",
            asr_model=model,
        )
    return int(audio_id)


def test_push_sends_new_segment_and_audio_then_is_idempotent(tmp_path: Path) -> None:
    store = Store.memory()
    _seed_segment(store, tmp_path)

    client = FakeClient()
    assert sync_push(store, client) == 1
    assert client.audio_pushed == [("usb", "seg-0001.opus")]
    assert len(client.segments) == 1
    assert client.segments[0].source_name == "Household"  # real source name, not the id

    # second pass: nothing changed → nothing pushed, and no re-upload of the blob
    assert sync_push(store, client) == 0
    assert client.audio_pushed == [("usb", "seg-0001.opus")]


def test_push_resends_a_re_derived_segment_but_not_its_audio(tmp_path: Path) -> None:
    store = Store.memory()
    audio_id = _seed_segment(store, tmp_path)
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


def test_a_processed_speechless_segment_is_mirrored_once(tmp_path: Path) -> None:
    # The watermark can't see it (no turn ids), but the fleet's quiet review must:
    # a segment nobody can see is a segment nobody can ever sweep.
    store = Store.memory()
    audio_id = _seed_segment(store, tmp_path, model=None)  # VAD heard nothing
    store.mark_transcribed(audio_id)

    client = FakeClient()
    assert sync_push(store, client) == 1
    assert client.audio_pushed == [("usb", "seg-0001.opus")]
    assert len(client.segments) == 1
    assert client.segments[0].turns == []  # honestly speechless
    # stamped pushed: the next pass sends nothing
    assert sync_push(store, client) == 0


def test_an_unprocessed_segment_is_not_mirrored_yet(tmp_path: Path) -> None:
    # Untranscribed = the worker hasn't listened yet; mirroring it would race the
    # pipeline. It ships once processed.
    store = Store.memory()
    _seed_segment(store, tmp_path, model=None)
    client = FakeClient()
    assert sync_push(store, client) == 0
    assert client.segments == []


def test_a_mirrored_segment_whose_file_vanished_does_not_wedge_the_queue(
    tmp_path: Path,
) -> None:
    store = Store.memory()
    audio_id = _seed_segment(store, tmp_path, model=None)
    store.mark_transcribed(audio_id)
    (tmp_path / "usb" / "seg-0001.opus").unlink()

    client = FakeClient()
    assert sync_push(store, client) == 0  # nothing sent — but stamped as handled
    assert client.segments == []
    assert sync_push(store, client) == 0  # and never retried


def test_push_sends_day_summaries() -> None:
    store = Store.memory()
    store.set_day_summary("2026-07-11", "a quiet day", model="qwen")
    client = FakeClient()
    sync_push(store, client)
    assert [(s.day, s.text) for s in client.summaries] == [
        ("2026-07-11", "a quiet day")
    ]


def _add_live(store: Store, at_s: float, text: str) -> int:
    return store.add_transcript_segment(
        audio_segment_id=None,
        start=BASE + timedelta(seconds=at_s),
        end=BASE + timedelta(seconds=at_s + 1),
        text=text,
        asr_model="live",
    )


def test_push_live_sends_visible_live_turns_then_is_idempotent() -> None:
    store = Store.memory()
    _add_live(store, 1, "hello")
    _add_live(store, 2, "world")
    client = FakeClient()

    assert push_live_turns(store, client) == 2
    assert [t.text for t in client.live] == ["hello", "world"]
    assert all(t.asr_model == "live" for t in client.live)
    # Watermark: a second pass with nothing new sends nothing.
    assert push_live_turns(store, client) == 0
    assert len(client.live) == 2
    # A newly-arrived live turn is the only thing the next pass sends.
    _add_live(store, 3, "again")
    assert push_live_turns(store, client) == 1
    assert client.live[-1].text == "again"


def test_push_live_skips_reconciled_turns() -> None:
    # A live turn the archive already reconciled (hidden) is the clean segment's job to
    # carry; the instant-feed push must not re-send it.
    store = Store.memory()
    keep = _add_live(store, 1, "visible")
    gone = _add_live(store, 2, "reconciled")
    store.hide(gone, "live-reconciled")
    client = FakeClient()

    assert push_live_turns(store, client) == 1
    assert [t.text for t in client.live] == ["visible"]
    assert keep  # (silence the unused-var check; the visible one is what shipped)


def _seed_clustered_turn(store: Store, tmp_path: Path, cluster: str) -> int:
    """Seed one machine turn tagged with a diarization cluster but not yet named — the
    state a freshly-pushed meeting is in on the Mac before its labels come back."""
    audio_id = _seed_segment(store, tmp_path, model=None)
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=5),
        text="hello",
        asr_model="turbo",
        speaker_cluster=cluster,
    )
    return audio_id


def test_pull_labels_names_a_voice_from_the_fleet_then_is_idempotent(
    tmp_path: Path,
) -> None:
    # The UI is on the fleet: a person names SPEAKER_00 there. Until it is replayed
    # here, the master archive and the voiceprint enrolment never learn the name.
    store = Store.memory()
    audio_id = _seed_clustered_turn(store, tmp_path, "SPEAKER_00")
    client = FakeClient()
    client.labels = [LabelOut(source_id="usb", cluster="SPEAKER_00", name="Dr. Voss")]

    assert pull_labels(store, client) == 1
    turns = store.visible_machine_turns_for_audio(audio_id)
    assert [t.speaker_label for t in turns] == ["Dr. Voss"]

    # A second pass changes nothing — the store already names that voice the same way.
    assert pull_labels(store, client) == 0


def test_pull_labels_applies_a_rename(tmp_path: Path) -> None:
    # The fleet is authoritative for human input: a corrected name overrides the old.
    store = Store.memory()
    audio_id = _seed_clustered_turn(store, tmp_path, "SPEAKER_00")
    client = FakeClient()
    client.labels = [LabelOut(source_id="usb", cluster="SPEAKER_00", name="Dr Lee")]
    assert pull_labels(store, client) == 1

    client.labels = [LabelOut(source_id="usb", cluster="SPEAKER_00", name="Dr. Voss")]
    assert pull_labels(store, client) == 1
    turns = store.visible_machine_turns_for_audio(audio_id)
    assert [t.speaker_label for t in turns] == ["Dr. Voss"]


def test_pull_labels_ignores_a_voice_this_machine_does_not_have(tmp_path: Path) -> None:
    # A naming for a cluster with no turns here (a source the Mac hasn't got) applies to
    # nothing and is not counted — no crash, no phantom.
    store = Store.memory()
    _seed_clustered_turn(store, tmp_path, "SPEAKER_00")
    client = FakeClient()
    client.labels = [LabelOut(source_id="usb", cluster="SPEAKER_99", name="Nobody")]
    assert pull_labels(store, client) == 0


class _OldFleetClient(FakeClient):
    """A fleet that predates GET /sync/labels: the endpoint 404s."""

    @override
    def fetch_labels(self) -> list[LabelOut]:
        request = httpx.Request("GET", "http://fleet/sync/labels")
        response = httpx.Response(404, request=request)
        raise httpx.HTTPStatusError("not found", request=request, response=response)


def test_pull_labels_tolerates_a_fleet_without_the_endpoint(tmp_path: Path) -> None:
    # The Mac runs live source, so pull_labels is active before the fleet is redeployed.
    # A 404 must not fail the sync pass (the push already ran); it self-heals on deploy.
    store = Store.memory()
    _seed_clustered_turn(store, tmp_path, "SPEAKER_00")
    assert pull_labels(store, _OldFleetClient()) == 0


def test_pull_labels_reraises_a_real_error(tmp_path: Path) -> None:
    # Only a missing endpoint is tolerated; an auth/server failure is real and surfaces.
    class _BrokenFleet(FakeClient):
        @override
        def fetch_labels(self) -> list[LabelOut]:
            request = httpx.Request("GET", "http://fleet/sync/labels")
            response = httpx.Response(401, request=request)
            raise httpx.HTTPStatusError("nope", request=request, response=response)

    store = Store.memory()
    with pytest.raises(httpx.HTTPStatusError):
        pull_labels(store, _BrokenFleet())
