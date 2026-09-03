"""The cross-process seams, exercised for real (#1345).

Every incident that actually hurt ran BETWEEN processes — the producer pipe, the
ffmpeg segmenter, filename stamping, the scan, ingest, the store — seams the
unit fakes are blind to by construction. This drives the real capture pipeline
(runner.record with a LAVFI producer: real pipes, real ffmpeg, real files, real
metered pump) and then a real worker pass (real probe/scan, real working-copy
derivation) with exactly one fake at the model boundary. Slow by suite
standards (~6 s of wall-clock streaming); that is the price of the seams.
"""

from __future__ import annotations

import time
from pathlib import Path

from recall.asr import AsrResult, AsrSegment
from recall.capture import ALIVE_FILE, CaptureConfig, parse_segment_start
from recall.runner import record
from recall.sources import AudioSource, SourceKind
from recall.store import Store
from recall.worker import process_pending


def test_capture_to_transcript_through_the_real_pipeline(tmp_path: Path) -> None:
    # A synthetic tone, streamed realtime through the SAME record() the capture
    # agent runs — only the device differs (LAVFI instead of CoreAudio).
    source = AudioSource(
        id="e2e",
        name="e2e",
        kind=SourceKind.LAVFI,
        spec="sine=frequency=440:sample_rate=48000",
    )
    config = CaptureConfig(segment_seconds=2)
    rc = record(source, config, tmp_path, max_seconds=5)
    assert rc == 0, "the segmenter should finalise cleanly on producer EOF"

    seg_dir = tmp_path / "e2e"
    files = sorted(seg_dir.glob("e2e-*"))
    assert len(files) >= 2, f"5s at 2s rotation should leave >=2 segments: {files}"
    # UTC-named and parseable — the invariant every downstream reader leans on.
    stamps = [parse_segment_start(f.name) for f in files]
    assert stamps == sorted(stamps)
    # The metered pump proved real signal reached the segmenter: the liveness
    # marker exists (a sine at full scale clears the silence floor).
    assert (seg_dir / ALIVE_FILE).exists()

    # The worker pass: real scan (ffprobe on the real Opus files), real working
    # copy (ffmpeg), fake ASR only. min_age=0 + a future 'now' so the files
    # count as settled.
    def transcriber(_audio: Path) -> AsrResult:
        return AsrResult(
            language="en",
            language_confidence=0.9,
            segments=(
                AsrSegment(
                    start=0.0,
                    end=1.5,
                    text="a tone from the pipeline",
                    avg_logprob=-0.2,
                    no_speech_prob=0.0,
                ),
            ),
        )

    store = Store.open(tmp_path / "recall.sqlite")
    try:
        written = process_pending(
            store,
            tmp_path,
            source,
            transcriber,
            model_name="fake-e2e",
            min_age_seconds=0.0,
            now=time.time() + 3600,
        )
        assert written >= 2, "every settled segment should yield its turn"
        # The turns are visible, searchable, and anchored to real audio rows.
        hits = store.search("pipeline")
        assert len(hits) >= 2
        assert all(h.audio_segment_id is not None for h in hits)
        # And nothing is left pending: the pass is idempotent-complete.
        pending = [s for s in store.pending_audio_segments() if s.source_id == "e2e"]
        assert pending == []
        # A second pass writes nothing (transcribed segments are never redone).
        again = process_pending(
            store,
            tmp_path,
            source,
            transcriber,
            model_name="fake-e2e",
            min_age_seconds=0.0,
            now=time.time() + 3600,
        )
        assert again == 0
    finally:
        store.close()
