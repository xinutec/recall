"""`recall capture-trace`: the merged, time-ordered capture timeline — the Phase-1
deliverable of docs/capture-loss-plan.md. After a controlled resume this one command
says which source recorded what, at what level, and when — events (mirror applications,
resume/pause, phone connects with measured levels, dead windows) interleaved with the
audio segments actually written."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest

from recall import capture_control, cli
from recall.sources import AudioSource, SourceKind
from recall.store import Store
from recall.timeline import Segment


def _seed(tmp_path: Path, now: datetime) -> None:
    store = Store.open(tmp_path / "recall.sqlite")
    try:
        store.add_source(
            AudioSource(id="pixel9", name="pixel9", kind=SourceKind.TCP_PCM, spec="")
        )
        store.add_capture_event(
            capture_control.CaptureEventKind.MIRROR_APPLIED,
            utc=now - timedelta(minutes=5),
            detail="running",
        )
        store.add_capture_event(
            capture_control.CaptureEventKind.INGEST_CONNECT,
            utc=now - timedelta(minutes=4),
            source_id="pixel9",
        )
        store.add_audio_segment(
            Segment(
                source_id="pixel9",
                sequence=0,
                start=now - timedelta(minutes=3),
                end=now - timedelta(minutes=2),
                path="pixel9/pixel9-x.opus",
                sample_rate=48000,
                channels=1,
            )
        )
        store.add_capture_event(
            capture_control.CaptureEventKind.INGEST_DISCONNECT,
            utc=now - timedelta(minutes=1),
            source_id="pixel9",
            detail='{"bytes": 0, "peak_db": null}',
        )
    finally:
        store.close()


def test_capture_trace_merges_events_and_segments_in_time_order(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    _seed(tmp_path, datetime.now(UTC))
    rc = cli.main(["capture-trace", "--out", str(tmp_path), "--minutes", "10"])
    assert rc == 0
    out = capsys.readouterr().out
    # Chronological: intent applied -> phone connected -> its segment -> disconnect.
    assert (
        out.index("mirror_applied")
        < out.index("ingest_connect")
        < out.index("segment")
        < out.index("ingest_disconnect")
    )
    # The measured evidence is shown, not summarised away.
    assert '"peak_db": null' in out
    assert "60s" in out  # the segment's coverage duration


def test_capture_trace_shows_fresh_unindexed_files_including_stubs(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    # The worker's min-age guard means a just-written segment isn't indexed for
    # minutes — and a zero-byte stub is DELETED once it is noticed. The trace must
    # show these files live, from disk, or a mid-sitting diagnosis is blind.
    now = datetime.now(UTC)
    _seed(tmp_path, now)
    stub = tmp_path / "pixel9" / f"pixel9-{now:%Y%m%dT%H%M%S}.opus"
    stub.parent.mkdir()
    stub.touch()  # zero bytes — the dead-window signature
    rc = cli.main(["capture-trace", "--out", str(tmp_path), "--minutes", "10"])
    assert rc == 0
    out = capsys.readouterr().out
    assert stub.name in out
    assert "0 bytes, not yet indexed" in out


def test_capture_trace_scopes_to_the_window(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    now = datetime.now(UTC)
    _seed(tmp_path, now - timedelta(hours=2))  # everything is older than the window
    rc = cli.main(["capture-trace", "--out", str(tmp_path), "--minutes", "10"])
    assert rc == 0
    assert "no capture events or segments" in capsys.readouterr().out


def test_capture_trace_on_an_empty_archive_says_so(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    rc = cli.main(["capture-trace", "--out", str(tmp_path)])
    assert rc == 0
    assert "no capture events or segments" in capsys.readouterr().out
