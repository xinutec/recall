"""The transcription worker: index + transcribe pending, idempotent, age-guarded."""

from __future__ import annotations

import logging
import os
import subprocess
import time
from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest

from recall.asr import AsrResult, AsrSegment
from recall.probe import scan_segments
from recall.sources import AudioSource, SourceKind
from recall.store import Store
from recall.timeline import Segment
from recall.worker import (
    discover_source_ids,
    order_by_yield,
    process_all,
    process_pending,
    reconcile_live,
)

USB = AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")


def _capture_two_segments(audio_dir: Path) -> None:
    """Generate two 1s FLAC segments named for the source (the dir name)."""
    audio_dir.mkdir(parents=True, exist_ok=True)
    source_id = audio_dir.name
    for ts in ("20260613T120000", "20260613T120001"):
        subprocess.run(
            [
                "ffmpeg",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000",
                "-t",
                "1",
                "-ac",
                "1",
                "-c:a",
                "flac",
                str(audio_dir / f"{source_id}-{ts}.flac"),
            ],
            check=True,
        )


def _stub_transcriber(_audio: Path) -> AsrResult:
    return AsrResult(
        language="en",
        language_confidence=0.9,
        segments=(
            AsrSegment(
                start=0.0,
                end=1.0,
                text="hello world",
                avg_logprob=-0.2,
                no_speech_prob=0.0,
            ),
        ),
    )


def test_worker_transcribes_pending_then_is_idempotent(tmp_path: Path) -> None:
    _capture_two_segments(tmp_path / "usb")
    store = Store.memory()

    # `now` far in the future so the min-age guard treats both files as complete
    future = 9_999_999_999.0
    written = process_pending(
        store,
        tmp_path,
        USB,
        _stub_transcriber,
        model_name="stub",
        now=future,
    )
    assert written == 2
    assert len(store.search("hello")) == 2

    # second pass: nothing new to do
    again = process_pending(
        store,
        tmp_path,
        USB,
        _stub_transcriber,
        model_name="stub",
        now=future,
    )
    assert again == 0


def test_discover_source_ids_needs_segment_files(tmp_path: Path) -> None:
    # A real source dir holds `<id>-<timestamp>` segment files; dirs without them
    # (the refine `work` dir, fine-tune pilot output) must not register as sources.
    (tmp_path / "usb").mkdir()
    (tmp_path / "usb" / "usb-20260619T120000.opus").write_bytes(b"x")
    (tmp_path / "phone").mkdir()
    (tmp_path / "phone" / "phone-20260619T120000.opus").write_bytes(b"x")
    (tmp_path / "work").mkdir()  # refine work dir — no segments
    # fine-tune pilot output — clips/ + manifest, no segment files
    pilot = tmp_path / "pilot-finetune"
    (pilot / "clips").mkdir(parents=True)
    (pilot / "manifest.jsonl").write_bytes(b"{}")
    assert discover_source_ids(tmp_path) == ["phone", "usb"]


def test_process_all_handles_multiple_sources(tmp_path: Path) -> None:
    _capture_two_segments(tmp_path / "usb")
    _capture_two_segments(tmp_path / "phone")
    store = Store.memory()
    written = process_all(
        store,
        tmp_path,
        _stub_transcriber,
        model_name="stub",
        now=9_999_999_999.0,
    )
    assert written == 4  # two segments from each of the two sources


def test_discovered_sources_are_registered_as_discovered_not_as_microphones(
    tmp_path: Path,
) -> None:
    # The worker finds a directory of audio; it does NOT know what produced it. Claiming
    # COREAUDIO here is what branded a copied-in meeting a microphone: `add_source` is
    # INSERT OR IGNORE, so the first registrar wins for good and the real one (the
    # upload path) could never correct it.
    _capture_two_segments(tmp_path / "meeting-20260731-0916")
    store = Store.memory()
    process_all(
        store, tmp_path, _stub_transcriber, model_name="stub", now=9_999_999_999.0
    )
    assert store.source_kind("meeting-20260731-0916") is SourceKind.DISCOVERED


def test_an_authoritative_registration_corrects_the_worker_guess(
    tmp_path: Path,
) -> None:
    # The whole point of guessing quietly: whoever actually knows gets to say so.
    _capture_two_segments(tmp_path / "meeting-20260731-0916")
    store = Store.memory()
    process_all(
        store, tmp_path, _stub_transcriber, model_name="stub", now=9_999_999_999.0
    )
    store.register_source(
        AudioSource(
            id="meeting-20260731-0916",
            name="Meeting 2026-07-31 09:16",
            kind=SourceKind.UPLOAD,
            spec="",
        )
    )
    assert store.source_kind("meeting-20260731-0916") is SourceKind.UPLOAD


def test_worker_skips_in_progress_segment(tmp_path: Path) -> None:
    audio_dir = tmp_path / "usb"
    _capture_two_segments(audio_dir)
    now = time.time()
    # one segment finished a while ago; the other is being written right now
    os.utime(audio_dir / "usb-20260613T120000.flac", (now - 1000, now - 1000))
    os.utime(audio_dir / "usb-20260613T120001.flac", (now, now))
    store = Store.memory()

    written = process_pending(
        store,
        tmp_path,
        USB,
        _stub_transcriber,
        model_name="stub",
        min_age_seconds=120.0,
        now=now,
    )
    # only the older segment is transcribed; the fresh one is skipped
    assert written == 1


def test_worker_does_not_index_an_in_progress_segment(tmp_path: Path) -> None:
    # The in-progress file must be skipped at INDEX time, not just transcribe time:
    # a partial Opus/FLAC probes fine and yields a truncated duration, and once the
    # row exists its path is in `known`, so the short end_utc would stand forever.
    audio_dir = tmp_path / "usb"
    _capture_two_segments(audio_dir)
    now = time.time()
    os.utime(audio_dir / "usb-20260613T120000.flac", (now - 1000, now - 1000))
    os.utime(audio_dir / "usb-20260613T120001.flac", (now, now))
    store = Store.memory()

    process_pending(
        store,
        tmp_path,
        USB,
        _stub_transcriber,
        model_name="stub",
        min_age_seconds=120.0,
        now=now,
    )
    indexed = {path for _, path in store.audio_segment_paths()}
    assert indexed == {str(audio_dir / "usb-20260613T120000.flac")}

    # Once the file is old enough it gets indexed (at its final duration) and done.
    written = process_pending(
        store,
        tmp_path,
        USB,
        _stub_transcriber,
        model_name="stub",
        min_age_seconds=120.0,
        now=now + 1000,
    )
    assert written == 1
    assert len(store.audio_segment_paths()) == 2


def test_worker_survives_a_pending_segment_whose_file_is_gone(tmp_path: Path) -> None:
    # A pending row whose file has vanished from disk must not crash the pass —
    # the whole pipeline (and the pause auto-resume that rides on the worker loop)
    # would wedge in a launchd crash-loop. The other segments still get done.
    audio_dir = tmp_path / "usb"
    _capture_two_segments(audio_dir)
    store = Store.memory()
    future = 9_999_999_999.0
    process_pending(
        store, tmp_path, USB, _stub_transcriber, model_name="stub", now=future
    )

    # A third file appears, gets indexed, then vanishes before transcription.
    ghost = audio_dir / "usb-20260613T120002.flac"
    (audio_dir / "usb-20260613T120000.flac").rename(ghost)
    known = frozenset(path for _, path in store.audio_segment_paths())
    for segment in scan_segments(audio_dir, "usb", known=known):
        store.add_audio_segment(segment)
    ghost.unlink()

    written = process_pending(
        store, tmp_path, USB, _stub_transcriber, model_name="stub", now=future
    )
    assert written == 0  # nothing crashed; the ghost row is simply skipped


def test_reconcile_live_hides_rather_than_superseding(tmp_path: Path) -> None:
    # Superseding a live turn by an UNRELATED archive turn corrupts deep links:
    # current_version() would resolve the live turn to a different utterance's
    # text. Reconciled live turns are hidden instead — gone from views, but still
    # resolving to themselves.
    base = datetime(2026, 6, 13, 12, 0, 0, tzinfo=UTC)
    store = Store.memory()
    store.add_source(USB)
    audio_id = store.add_audio_segment(
        Segment(
            source_id="usb",
            sequence=0,
            start=base,
            end=base + timedelta(seconds=60),
            path="x",
            sample_rate=48000,
            channels=1,
        )
    )
    live_id = store.add_transcript_segment(
        audio_segment_id=None,
        start=base,
        end=base + timedelta(seconds=1),
        text="live words",
        asr_model="live",
    )
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=base + timedelta(seconds=2),
        end=base + timedelta(seconds=3),
        text="different archive words",
        asr_model="whisper",
    )
    store.mark_transcribed(audio_id)  # reconcile runs after the segment is processed

    assert reconcile_live(store) == 1

    resolved = store.current_version(live_id)
    assert resolved is not None
    assert resolved.id == live_id  # still resolves to itself…
    assert resolved.text == "live words"  # …never to an unrelated utterance
    visible = {
        s.text for s in store.segments_in_range(base, base + timedelta(seconds=10))
    }
    assert visible == {"different archive words"}


def test_the_worker_removes_dead_capture_files_and_keeps_recoverable_ones(
    tmp_path: Path, caplog: pytest.LogCaptureFixture
) -> None:
    """A dead-capture file holds no usable audio and marks the instant capture died: a
    zero-byte file (capture wrote nothing), or a tiny truncated header (ffprobe refuses
    it, ~136 bytes). Both are removed and journalled with a dead-window event. Left in
    place they were re-probed every pass forever and, unindexed, invisible to every
    archive check. A LARGER unreadable file might hold a recoverable audio body behind a
    corrupt header, so it is kept — recorded once, then skipped (never re-probed).
    """
    audio_dir = tmp_path / "usb"
    _capture_two_segments(audio_dir)
    empty = audio_dir / "usb-20260613T120010.flac"
    empty.touch()  # capture opened it and wrote nothing
    truncated = audio_dir / "usb-20260613T120020.flac"
    truncated.write_bytes(b"fLaC truncated mid-write")  # header only, no audio pages
    big_corrupt = audio_dir / "usb-20260613T120030.flac"
    big_corrupt.write_bytes(b"fLaC truncated mid-write " * 200)  # ~5 kB, may hold audio

    store = Store.memory()
    with caplog.at_level(logging.WARNING):
        process_pending(
            store,
            tmp_path,
            USB,
            _stub_transcriber,
            model_name="stub",
            min_age_seconds=0.0,
        )

    # Both dead-capture files are removed and journalled as dead windows.
    assert not empty.exists()
    assert not truncated.exists()
    assert "capture died here" in caplog.text
    events = store.capture_events_since(datetime(2026, 1, 1, tzinfo=UTC))
    assert {empty.name, truncated.name} <= {e.detail for e in events}

    # The larger corrupt file is kept (may hold audio), recorded so it isn't re-probed.
    assert big_corrupt.exists()
    assert "unreadable capture file" in caplog.text
    assert len(store.audio_segment_paths()) == 2  # only the two real recordings

    # Second pass: the kept file is skipped (in `known`) — NOT re-probed/re-logged
    # — the loop that spammed 10k+ log lines for one file is gone.
    caplog.clear()
    with caplog.at_level(logging.WARNING):
        process_pending(
            store,
            tmp_path,
            USB,
            _stub_transcriber,
            model_name="stub",
            min_age_seconds=0.0,
        )
    assert "unreadable capture file" not in caplog.text
    assert big_corrupt.exists()


def test_every_source_is_indexed_even_when_transcription_is_capped(
    tmp_path: Path,
) -> None:
    # #1365: sources were served alphabetically and each was drained fully before
    # the next, so under multi-mic load the later ones starved — measured live
    # 2026-09-03 with usb 2h and pixel5 3h unindexed while iphone11 stayed current.
    # Indexing is cheap (ffprobe) and transcription is expensive (Whisper), so a
    # pass now INDEXES every source before spending its transcription budget.
    # Every segment must therefore have a row after one pass, whatever the cap.
    for name in ("aaa-first", "zzz-last"):
        _capture_two_segments(tmp_path / name)
    store = Store.memory()
    process_all(
        store,
        tmp_path,
        _stub_transcriber,
        model_name="stub",
        now=9_999_999_999.0,
        max_transcribe_per_source=1,
    )
    rows = {sid for sid, _ in store.audio_segment_paths()}
    indexed = {p.split("/")[-2] for p in {p for _, p in store.audio_segment_paths()}}
    assert indexed == {"aaa-first", "zzz-last"}, "both sources indexed in one pass"
    assert len(rows) == 4  # every segment has a row


def test_the_transcription_budget_is_shared_not_drained_by_the_first_source(
    tmp_path: Path,
) -> None:
    # The fairness half: with a cap of one, the LAST source alphabetically must
    # still get a turn in the same pass — under the old drain-in-order loop it got
    # whatever was left, which under real load was nothing.
    for name in ("aaa-first", "zzz-last"):
        _capture_two_segments(tmp_path / name)
    store = Store.memory()
    process_all(
        store,
        tmp_path,
        _stub_transcriber,
        model_name="stub",
        now=9_999_999_999.0,
        max_transcribe_per_source=1,
    )
    # Each source has two segments and the cap is one, so a fair pass leaves
    # exactly one pending on EACH — not two pending on the alphabetically-later
    # one, which is what draining in order produced.
    still_pending = [s.source_id for s in store.pending_audio_segments()]
    assert sorted(still_pending) == ["aaa-first", "zzz-last"]


def test_indexing_completes_for_every_source_before_transcription_spends_time(
    tmp_path: Path,
) -> None:
    # The real shape of #1365, found by measuring the fix: capping transcription
    # per source is not enough when INDEXING still queues behind the previous
    # source's Whisper time. usb sorts last of 33 source dirs, so its rows waited
    # a whole cycle — two hours of the visit — even after the cap landed.
    # A pass now indexes every source first, then spends its transcription budget.
    # Proof: transcription blowing up on the first source must not stop the last
    # source's audio from being indexed.
    for name in ("aaa-first", "zzz-last"):
        _capture_two_segments(tmp_path / name)

    def exploding(_audio: Path) -> AsrResult:
        msg = "ASR down"
        raise RuntimeError(msg)

    store = Store.memory()
    with pytest.raises(RuntimeError, match="ASR down"):
        process_all(store, tmp_path, exploding, model_name="stub", now=9_999_999_999.0)
    indexed = {p.split("/")[-2] for _, p in store.audio_segment_paths()}
    assert indexed == {"aaa-first", "zzz-last"}, (
        "every source indexed before any transcription ran"
    )


def test_order_by_yield_serves_the_microphone_that_can_actually_hear() -> None:
    # #1388 stage 1. Four mics hear the same room and all four are transcribed, so
    # under a capacity deficit every mic lags equally and the timeline is current
    # from NONE of them. Measured 2026-09-03: iphone11 yielded ~107 chars per loud
    # segment where pixel5 yielded 4.5 — the mics are not equals. Serve the best
    # first so the timeline is COMPLETE from the mic that can hear, rather than
    # partial from all four. Nothing is skipped; the rest follow in the same pass.
    yields = {"pixel5": 4.5, "usb": 37.6, "iphone11": 107.3}
    assert order_by_yield(["pixel5", "usb", "iphone11"], yields) == [
        "iphone11",
        "usb",
        "pixel5",
    ]


def test_a_source_with_no_history_is_served_first_not_last() -> None:
    # A newly added recorder has no yield yet. Sorting it last would starve it
    # exactly when we most want to learn what it can hear — and Pippijn is adding
    # recorders. Unknown goes FIRST, so it earns a history to be judged on.
    yields = {"usb": 37.6}
    assert order_by_yield(["usb", "new-mic"], yields) == ["new-mic", "usb"]


def test_order_by_yield_is_stable_for_equal_scores() -> None:
    # Ties keep discovery order, so the pass is deterministic run to run.
    yields = {"a": 5.0, "b": 5.0, "c": 5.0}
    assert order_by_yield(["a", "b", "c"], yields) == ["a", "b", "c"]


def test_a_lagging_source_is_judged_on_its_history_not_treated_as_new() -> None:
    # Caught by running the ranking against the real archive: pixel5 had nothing
    # transcribed in the recent window (it was simply BEHIND, not new), so it
    # looked unknown and jumped the queue ahead of the best mic — the opposite of
    # the point. Only a source with NO history at all is unknown; one with a past
    # is judged on it.
    recent = {"iphone11": 120.6}
    lifetime = {"iphone11": 100.0, "pixel5": 4.5}
    assert order_by_yield(
        ["pixel5", "iphone11", "brand-new"], recent, lifetime=lifetime
    ) == ["brand-new", "iphone11", "pixel5"]
