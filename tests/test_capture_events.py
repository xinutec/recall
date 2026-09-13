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
from recall.store import Store


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
