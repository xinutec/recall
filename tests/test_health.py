"""Is the recording actually recording? The check recall did not have in June."""

from __future__ import annotations

import os
from collections.abc import Sequence
from datetime import UTC, datetime, timedelta
from pathlib import Path

from recall.fleetwatch import COLLECTOR, INTERVAL_S, build_report, mint_ulid
from recall.health import (
    Check,
    Recorder,
    agent_checks,
    backup_check,
    capture_checks,
    mirror_check,
    recorders_on_disk,
)
from recall.sources import SourceKind

NOW = datetime(2026, 7, 13, 21, 0, 0, tzinfo=UTC)


def _mic(source_id: str, kind: SourceKind, ago: timedelta | None) -> Recorder:
    return Recorder(
        source_id=source_id,
        kind=kind,
        last_audio=None if ago is None else NOW - ago,
    )


def _verdicts(checks: Sequence[Check]) -> dict[str, str]:
    return {c.label: c.verdict for c in checks}


def _all_three(usb: timedelta | None, phones: timedelta | None) -> list[Recorder]:
    return [
        _mic("usb", SourceKind.COREAUDIO, usb),
        _mic("pixel9", SourceKind.TCP_PCM, phones),
        _mic("pixel5", SourceKind.TCP_PCM, phones),
    ]


def test_a_recording_house_passes() -> None:
    checks = capture_checks(
        _all_three(timedelta(minutes=1), timedelta(minutes=1)), now=NOW
    )
    assert _verdicts(checks) == {
        "usb": "pass",
        "pixel9": "pass",
        "pixel5": "pass",
        "recording": "pass",
    }


def test_every_microphone_silent_at_once_is_the_capture_process() -> None:
    """What actually happened, on the night of 22 June.

    Capture crash-looped — fourteen start attempts between 01:05 and 03:10 — and
    nothing was recorded for about ninety minutes. Three microphones do not fall
    silent together by coincidence: that is the process, or the machine it runs on,
    and it is the loudest thing this check can say.
    """
    checks = capture_checks(_all_three(timedelta(hours=2), timedelta(hours=2)), now=NOW)

    assert _verdicts(checks)["recording"] == "fail"
    detail = next(c for c in checks if c.label == "recording")
    assert "capture is not running" in detail.observed


def test_the_always_on_mic_has_no_excuse_for_silence() -> None:
    # The USB condenser is wired to the machine doing the recording. If it stops, the
    # recording has stopped — whatever the phones are doing.
    checks = capture_checks(
        _all_three(timedelta(hours=1), timedelta(minutes=1)), now=NOW
    )

    assert _verdicts(checks)["usb"] == "fail"
    assert _verdicts(checks)["recording"] == "pass"  # the phones are still recording


def test_a_phone_that_left_the_house_warns_but_does_not_cry_wolf() -> None:
    # A phone is carried out, runs flat, has its app closed. That is normal life, not a
    # fault — but it is still worth seeing, so it warns rather than passing in silence.
    checks = capture_checks(
        _all_three(timedelta(minutes=1), timedelta(hours=6)), now=NOW
    )

    assert _verdicts(checks)["pixel9"] == "warn"
    assert _verdicts(checks)["usb"] == "pass"
    assert _verdicts(checks)["recording"] == "pass"


def test_a_paused_recording_is_not_a_broken_one() -> None:
    # Pausing is deliberate and must never page anyone. It is still *shown*, with the
    # resume time: a pause nobody remembers is how a week of memory goes missing.
    checks = capture_checks(
        _all_three(timedelta(days=1), timedelta(days=1)),
        now=NOW,
        paused_until=NOW + timedelta(hours=20),
    )

    assert [c.verdict for c in checks] == ["skip"]
    assert "paused until" in checks[0].observed


def test_an_expired_pause_is_no_longer_an_excuse() -> None:
    # The pause auto-resumes. Once it has, silence means silence again.
    checks = capture_checks(
        _all_three(timedelta(hours=3), timedelta(hours=3)),
        now=NOW,
        paused_until=NOW - timedelta(minutes=1),
    )

    assert _verdicts(checks)["recording"] == "fail"


def test_a_mic_that_never_recorded_anything_is_not_quietly_fine() -> None:
    checks = capture_checks([_mic("usb", SourceKind.COREAUDIO, None)], now=NOW)

    assert _verdicts(checks)["usb"] == "fail"
    assert "no audio ever" in next(c for c in checks if c.label == "usb").observed


def test_health_is_read_from_the_disk_not_the_database(tmp_path: Path) -> None:
    """Capture writing files is the fact under test.

    The transcription pipeline can be hours behind without the microphone having missed
    a second — asking the database "when did we last see audio?" answers a different
    question, and would have called a healthy recorder dead every time the worker fell
    behind.
    """
    (tmp_path / "usb").mkdir()
    recent = tmp_path / "usb" / "usb-20260713T205900.opus"
    recent.write_bytes(b"audio")
    os_time = (NOW - timedelta(minutes=1)).timestamp()
    os.utime(recent, (os_time, os_time))
    (tmp_path / "pixel9").mkdir()  # a directory, but no audio in it

    recorders = recorders_on_disk(
        tmp_path,
        [("usb", SourceKind.COREAUDIO), ("pixel9", SourceKind.TCP_PCM)],
        now=NOW,
    )

    assert _verdicts(capture_checks(recorders, now=NOW))["usb"] == "pass"
    assert _verdicts(capture_checks(recorders, now=NOW))["pixel9"] == "warn"


def test_a_dead_window_of_empty_stubs_does_not_read_as_recording(
    tmp_path: Path,
) -> None:
    """Capture can run — rolling a fresh file every segment — while the device
    delivers only silence, leaving a trail of zero-byte stubs (a coreaudio startup
    dead-window). Counting those as audio is how a 13-minute dead window read as
    healthy. The newest *real* audio is an old session's, so the always-on mic fails.
    """
    (tmp_path / "usb").mkdir()
    old = tmp_path / "usb" / "usb-20260713T120000.opus"  # a past session's real audio
    old.write_bytes(b"audio")
    old_time = (NOW - timedelta(hours=3)).timestamp()
    os.utime(old, (old_time, old_time))
    for i in range(3):  # the current dead window: fresh but empty, one a minute
        stub = tmp_path / "usb" / f"usb-20260713T2059{i:02d}.opus"
        stub.write_bytes(b"")
        fresh = (NOW - timedelta(minutes=i)).timestamp()
        os.utime(stub, (fresh, fresh))

    recorders = recorders_on_disk(tmp_path, [("usb", SourceKind.COREAUDIO)], now=NOW)

    assert recorders[0].last_audio is not None
    # last_audio is the old session's file, not the fresh empty stubs
    assert NOW - recorders[0].last_audio >= timedelta(hours=2)
    assert _verdicts(capture_checks(recorders, now=NOW))["usb"] == "fail"


def test_the_report_declares_the_cadence_that_makes_a_dead_producer_visible() -> None:
    """The property this whole design rests on.

    fleetwatch renders a producer that stops reporting as Silent — red. So the case
    'the Mac died' needs no detector here: *not reporting is the report*. That only
    works if the cadence is declared, and matches how often the agent actually runs.
    """
    report = build_report(
        capture_checks(_all_three(timedelta(minutes=1), timedelta(minutes=1)), now=NOW),
        now=NOW,
        randomness=bytes(10),
    )
    payload = report.payload()

    assert payload["interval_s"] == INTERVAL_S
    assert payload["collector"] == COLLECTOR
    assert payload["schema"] == 1
    assert "source" not in payload  # fleetwatch stamps it from the token, not the body


def test_the_report_id_is_a_real_ulid() -> None:
    # fleetwatch uses it as an idempotency key and rejects anything that is not a ULID
    # (422). 26 chars of Crockford base32, timestamp-first, so ids sort by time.
    early = mint_ulid(NOW, bytes(10))
    later = mint_ulid(NOW + timedelta(seconds=1), bytes(10))

    assert len(early) == 26
    assert set(early) <= set("0123456789ABCDEFGHJKMNPQRSTVWXYZ")
    assert early < later  # lexicographic order is chronological order


def test_an_unloaded_agent_is_always_a_fault() -> None:
    # The agents self-gate: they park while capture is paused, they do not unload. So
    # "not loaded" never means "deliberately off" — it means something broke.
    checks = agent_checks([("com.pippijn.recall-capture", False), ("x", True)])

    assert _verdicts(checks) == {"com.pippijn.recall-capture": "fail", "x": "pass"}


def test_no_agents_installed_at_all_is_not_quietly_fine() -> None:
    assert agent_checks([])[0].verdict == "fail"


def test_a_silently_dead_backup_fails() -> None:
    """The archive's only unrecoverable failure is losing the one local copy.

    Transcripts are derived and can be rebuilt; the raw audio and the human corrections
    exist on one volume and nowhere else. The mirror stopped once, for nine days, and
    nothing said a word — so a stale mirror is a fail, not a warn.
    """
    assert backup_check(20.3, max_age_hours=48.0).verdict == "pass"
    assert backup_check(72.0, max_age_hours=48.0).verdict == "fail"
    assert backup_check(None, max_age_hours=48.0).verdict == "fail"
    assert "never completed" in backup_check(None, max_age_hours=48.0).observed


def test_mirror_check_passes_only_when_the_fleet_holds_everything() -> None:
    # "If the Mac dies the archive lives on Isis" is only true while this is zero;
    # a stuck count is a silently stopped mirror — the stalled-backup class of
    # failure, and the same verdict.
    ok = mirror_check(0, slack=timedelta(hours=1))
    assert (ok.section, ok.verdict) == ("sync", "pass")
    stalled = mirror_check(3, slack=timedelta(hours=1))
    assert stalled.verdict == "fail"
    assert "3" in stalled.observed
