"""Mac-side sync push — replicate the local archive to the fleet (the proposed split,
`docs/isis-migration.md`).

Runs on the Mac (the compute node). The Mac is a one-way WireGuard peer, so it
*initiates* everything: it pushes new and re-derived segments (audio + turns) and
settled day-summaries to the fleet's system of record. Idempotent throughout — the
fleet no-ops an unchanged push — with a transcript-id watermark, so each pass only
touches what changed since the last one: new segments AND refine re-derivations (which
mint new turn ids), never the whole archive. The audio blob is uploaded only when the
fleet lacks it.

The client is a Protocol, so the push logic is unit-tested with a fake in place of the
real `SyncClient` (which drags in the server framework).
"""

from __future__ import annotations

from pathlib import Path
from typing import Protocol

from recall.store import Store, TranscriptSegment
from recall.sync import SegmentIn, SegmentStoredOut, SummaryIn, TurnIn
from recall.timeline import Segment

WATERMARK_KEY = "sync_pushed_max_turn_id"
LIVE_WATERMARK_KEY = "sync_pushed_max_live_turn_id"
_SUMMARY_PUSH_LIMIT = 60


class PushTarget(Protocol):
    """What `sync_push` needs of a client — `SyncClient` satisfies it structurally."""

    def audio_present(self, source: str, name: str) -> bool: ...
    def push_audio(self, source: str, name: str, local_path: Path) -> bool: ...
    def push_segment(self, segment: SegmentIn) -> SegmentStoredOut: ...
    def push_summary(self, summary: SummaryIn) -> None: ...
    def push_live(self, turns: list[TurnIn]) -> int: ...


def _segment_in(
    store: Store, seg: Segment, turns: list[TranscriptSegment]
) -> SegmentIn:
    src = store.source(seg.source_id)
    return SegmentIn(
        source_id=seg.source_id,
        source_name=src.name if src else seg.source_id,
        kind=src.kind.value if src else "coreaudio",
        path=seg.path,
        start=seg.start.isoformat(),
        end=seg.end.isoformat(),
        sample_rate=seg.sample_rate,
        channels=seg.channels,
        turns=[
            TurnIn(
                start=t.start.isoformat(),
                end=t.end.isoformat(),
                text=t.text,
                asr_model=t.asr_model,
                language=t.language,
                asr_confidence=t.asr_confidence,
                speaker_cluster=t.speaker_cluster,
                provenance=t.provenance,
            )
            for t in turns
        ],
    )


def _live_turn_in(turn: TranscriptSegment) -> TurnIn:
    return TurnIn(
        start=turn.start.isoformat(),
        end=turn.end.isoformat(),
        text=turn.text,
        asr_model=turn.asr_model,
        language=turn.language,
    )


def push_live_turns(store: Store, client: PushTarget) -> int:
    """Push new visible live turns to the fleet's instant feed; returns how many were
    sent. Separate from the segment push and cheap (only unpushed visible live turns,
    id-watermarked), so it can run on a much shorter cadence to keep the feed live."""
    watermark = int(store.get_setting(LIVE_WATERMARK_KEY) or 0)
    turns = store.visible_live_turns_since(watermark)
    if not turns:
        return 0
    client.push_live([_live_turn_in(t) for t in turns])
    store.set_setting(LIVE_WATERMARK_KEY, str(max(int(t.id) for t in turns)))
    return len(turns)


def sync_push(store: Store, client: PushTarget) -> int:
    """Push everything changed since the last pass; returns the segment count sent."""
    watermark = int(store.get_setting(WATERMARK_KEY) or 0)
    high = watermark
    pushed = 0
    for audio_id in store.audio_segment_ids_with_machine_turns():
        turns = store.visible_machine_turns_for_audio(audio_id)
        seg_max = max((int(t.id) for t in turns), default=0)
        if seg_max <= watermark:
            continue  # unchanged since the last push
        seg = store.audio_segment(audio_id)
        if seg is None:
            continue
        name = Path(seg.path).name
        if not client.audio_present(seg.source_id, name):
            client.push_audio(seg.source_id, name, Path(seg.path))
        client.push_segment(_segment_in(store, seg, turns))
        pushed += 1
        high = max(high, seg_max)
    for day, text, model in store.recent_day_summaries(limit=_SUMMARY_PUSH_LIMIT):
        client.push_summary(SummaryIn(day=day, text=text, model=model))
    if high > watermark:
        store.set_setting(WATERMARK_KEY, str(high))
    return pushed
