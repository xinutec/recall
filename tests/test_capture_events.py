"""Durable capture-lifecycle events — the record that tells a deliberate pause-gap apart
from silently lost (unrecoverable) audio.

A timeline gap alone can't say whether audio is missing because capture was paused on
purpose or because it silently died. These events are that record: pauses, resumes,
and a `dead_window` written the moment the worker clears a zero-byte stub — before the
file (today the only evidence) is deleted.
"""

from __future__ import annotations

import threading
from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest

from recall import capture_control, cli
from recall.probe import Scan
from recall.store import Store
from recall.worker import _clear_dead_stubs


def _store(tmp_path: Path) -> Store:
    return Store.open(tmp_path / "recall.sqlite")


def test_add_and_read_a_capture_event(tmp_path: Path) -> None:
    store = _store(tmp_path)
    try:
        t = datetime(2026, 7, 15, 10, 0, tzinfo=UTC)
        store.add_capture_event(
            capture_control.CaptureEventKind.PAUSE,
            utc=t,
            source_id="usb",
            detail="until 11:00",
        )
        got = store.capture_events_since(datetime(2026, 7, 15, tzinfo=UTC))
        assert len(got) == 1
        assert got[0].kind == "pause"
        assert got[0].utc == t
        assert got[0].source_id == "usb"
        assert got[0].detail == "until 11:00"
    finally:
        store.close()


def test_capture_events_since_is_a_lower_bound(tmp_path: Path) -> None:
    store = _store(tmp_path)
    try:
        old = datetime(2026, 7, 15, 9, 0, tzinfo=UTC)
        new = datetime(2026, 7, 15, 11, 0, tzinfo=UTC)
        store.add_capture_event(capture_control.CaptureEventKind.RESUME, utc=old)
        store.add_capture_event(capture_control.CaptureEventKind.RESUME, utc=new)
        got = store.capture_events_since(datetime(2026, 7, 15, 10, 0, tzinfo=UTC))
        assert [e.utc for e in got] == [new]
    finally:
        store.close()


def test_capture_events_can_be_filtered_by_kind(tmp_path: Path) -> None:
    store = _store(tmp_path)
    try:
        base = datetime(2026, 7, 15, 10, 0, tzinfo=UTC)
        store.add_capture_event(capture_control.CaptureEventKind.PAUSE, utc=base)
        store.add_capture_event(
            capture_control.CaptureEventKind.DEAD_WINDOW,
            utc=base + timedelta(minutes=1),
        )
        store.add_capture_event(
            capture_control.CaptureEventKind.RESUME, utc=base + timedelta(minutes=2)
        )
        dead = store.capture_events_since(
            base, kinds=(capture_control.CaptureEventKind.DEAD_WINDOW,)
        )
        assert [e.kind for e in dead] == ["dead_window"]
    finally:
        store.close()


def test_capture_events_come_back_oldest_first(tmp_path: Path) -> None:
    store = _store(tmp_path)
    try:
        base = datetime(2026, 7, 15, 10, 0, tzinfo=UTC)
        # insert out of chronological order
        store.add_capture_event(
            capture_control.CaptureEventKind.RESUME, utc=base + timedelta(minutes=5)
        )
        store.add_capture_event(capture_control.CaptureEventKind.PAUSE, utc=base)
        got = store.capture_events_since(base)
        assert [e.kind for e in got] == ["pause", "resume"]
    finally:
        store.close()


def test_add_capture_event_rejects_a_naive_timestamp(tmp_path: Path) -> None:
    store = _store(tmp_path)
    try:
        with pytest.raises(ValueError, match="utc"):
            store.add_capture_event(
                capture_control.CaptureEventKind.PAUSE, utc=datetime(2026, 7, 15, 10, 0)
            )
    finally:
        store.close()


def test_clearing_a_dead_stub_records_a_durable_dead_window_then_deletes(
    tmp_path: Path,
) -> None:
    store = _store(tmp_path)
    try:
        usb = tmp_path / "usb"
        usb.mkdir()
        stub = usb / "usb-20260715T090200.opus"
        stub.write_bytes(b"")  # a zero-byte tombstone
        # A NEWER sibling exists, so the stub cannot be the open segment — clearable.
        (usb / "usb-20260715T090300.opus").write_bytes(b"x")
        _clear_dead_stubs(Scan(segments=[], empty=[stub], unreadable=[]), store, "usb")

        assert not stub.exists()  # the file is gone...
        events = store.capture_events_since(datetime(2026, 7, 15, tzinfo=UTC))
        assert len(events) == 1  # ...but the evidence survives
        event = events[0]
        assert event.kind == capture_control.CaptureEventKind.DEAD_WINDOW
        assert event.source_id == "usb"
        assert event.detail == "usb-20260715T090200.opus"
        # timestamped to WHEN capture died (from the filename), not when noticed
        assert event.utc == datetime(2026, 7, 15, 9, 2, 0, tzinfo=UTC)
    finally:
        store.close()


def test_the_newest_stub_is_never_touched_it_may_be_the_open_segment(
    tmp_path: Path,
) -> None:
    # ffmpeg writes a segment's bytes only when it CLOSES (rotation or EOF) — measured
    # live 2026-07-16: the current segment sat at 0 bytes, held open, for 3 minutes
    # (lsof confirmed the open fd). Unlinking it would send the eventual flush to a
    # deleted inode: silent, unrecoverable loss. So the newest file of a source is
    # never cleared, and no dead_window is recorded for it (the verdict isn't in yet).
    store = _store(tmp_path)
    try:
        usb = tmp_path / "usb"
        usb.mkdir()
        stub = usb / "usb-20260715T090200.opus"
        stub.write_bytes(b"")  # zero bytes, but possibly still open by ffmpeg
        _clear_dead_stubs(Scan(segments=[], empty=[stub], unreadable=[]), store, "usb")

        assert stub.exists()  # left alone
        assert store.capture_events_since(datetime(2026, 7, 15, tzinfo=UTC)) == []
    finally:
        store.close()


def test_a_stub_cut_short_by_a_deliberate_pause_is_not_recorded_as_lost_speech(
    tmp_path: Path,
) -> None:
    """Turning capture off mid-segment leaves exactly what a dead device leaves.

    Observed 2026-07-22: capture resumed at 11:19:34, ffmpeg opened its first segment
    at 11:19:38, and the pause landed 0.4s later. The header-only stub read as a dead
    window and held the loss check red for 48 hours — over an act the household chose.
    """
    store = _store(tmp_path)
    try:
        usb = tmp_path / "usb"
        usb.mkdir()
        stub = usb / "usb-20260715T090200.opus"
        stub.write_bytes(b"")
        (usb / "usb-20260715T090300.opus").write_bytes(b"x")  # newer: stub is clearable
        store.add_capture_event(
            capture_control.CaptureEventKind.PAUSE,
            utc=datetime(2026, 7, 15, 9, 2, 0, 400_000, tzinfo=UTC),
            source_id="usb",
        )
        _clear_dead_stubs(Scan(segments=[], empty=[stub], unreadable=[]), store, "usb")

        assert not stub.exists()  # the empty file still goes...
        events = store.capture_events_since(datetime(2026, 7, 15, tzinfo=UTC))
        assert [e.kind for e in events] == ["pause"]  # ...the pause is the whole story
    finally:
        store.close()


def test_a_pause_long_after_the_stub_does_not_explain_it(tmp_path: Path) -> None:
    """Only a pause inside the stub's own segment window can have killed it. A pause
    ten minutes later is a different act, and the dead window is still real loss."""
    store = _store(tmp_path)
    try:
        usb = tmp_path / "usb"
        usb.mkdir()
        stub = usb / "usb-20260715T090200.opus"
        stub.write_bytes(b"")
        (usb / "usb-20260715T090300.opus").write_bytes(b"x")
        store.add_capture_event(
            capture_control.CaptureEventKind.PAUSE,
            utc=datetime(2026, 7, 15, 9, 12, 0, tzinfo=UTC),
            source_id="usb",
        )
        _clear_dead_stubs(Scan(segments=[], empty=[stub], unreadable=[]), store, "usb")

        events = store.capture_events_since(datetime(2026, 7, 15, tzinfo=UTC))
        assert [e.kind for e in events] == ["dead_window", "pause"]
    finally:
        store.close()


def test_the_supervisor_records_a_resume_when_capture_starts(tmp_path: Path) -> None:
    # Not paused (no pause file), so capture starts immediately: it must mark a `resume`
    # — the ground-truth start of an active span the loss check reconciles gaps against.
    recorded: list[str] = []
    done = threading.Event()

    def record_event(kind: str, utc: datetime) -> None:
        recorded.append(kind)
        done.set()

    def run_once(_should_stop: object) -> int:
        return 0  # producer EOF immediately (a non-pause exit) → return

    rc = cli._serve_paused_aware(tmp_path, run_once, record_event=record_event)
    assert rc == 0
    assert done.wait(2)  # the event is written on a daemon thread
    assert recorded == [capture_control.CaptureEventKind.RESUME]


def test_an_unreadable_stub_is_kept_and_not_recorded_as_dead(tmp_path: Path) -> None:
    # Non-empty but unreadable may still hold audio: never deleted, never a dead-window.
    store = _store(tmp_path)
    try:
        usb = tmp_path / "usb"
        usb.mkdir()
        bad = usb / "usb-20260715T090200.opus"
        bad.write_bytes(b"not really opus")
        _clear_dead_stubs(Scan(segments=[], empty=[], unreadable=[bad]), store, "usb")
        assert bad.exists()  # kept
        assert store.capture_events_since(datetime(2026, 7, 15, tzinfo=UTC)) == []
    finally:
        store.close()
